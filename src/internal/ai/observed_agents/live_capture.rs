//! Live-capture provider capability (ADR-ACF-10, ACF-18/ACF-20).
//!
//! The shared live pipeline (`capture::live_pipeline`) and checkpoint writers
//! (`capture::live_checkpoint`) stay provider-neutral: every provider-specific
//! live decision -- the capture budget used when the host supplied none, the
//! deterministic transcript candidate, the live coverage normalizer,
//! child-transcript discovery and the trusted transcript exporter -- is
//! answered here through [`LiveCaptureProvider`]. Source/discovery methods receive
//! scope-verified facts ([`LiveCaptureContext`]) and a monotonic deadline. The
//! pure hint extractor instead borrows the parsed, unverified hook envelope;
//! its bounded output is only a candidate and grants no source authority.
//! They never see SQL, refs, a capture scope, a commit deadline, a store or an
//! export-job lease, and they never open, stat or read a source themselves:
//! the snapshot service owns that boundary. A [`LiveTranscriptExporter`]
//! returns only an authorized `TranscriptSource::Bytes`; the capture layer
//! holds the export-job runner lease around the call (ACF-20).
//!
//! The hook entry resolves one [`LiveCaptureBinding`] per callback from the
//! provider's typed `HookProviderIdentity`; `super::live_capture_for` is the
//! only kind -> capability lookup, following the `truncator_for` precedent.
//! It is a capability accessor, not a second provider registry.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Result;
use async_trait::async_trait;

use super::{
    AgentKind, ExportAuthorized, NormalizedTurn, ObservedAgent, RedactedBytes,
    TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptSource, agent_for, claude_jsonl_session_matches,
    claude_session_file_candidate, live_capture_for, normalize_claude_transcript,
    normalize_codex_rollout, normalize_opencode_export,
    opencode_export::{ExportLimits, authorized_trusted_sandboxed_export_until},
    reasoning::note_claude_attached_duplicates,
};
use crate::internal::ai::{
    hooks::{
        lifecycle::{LifecycleEventKind, SessionHookEnvelope},
        provider::HookProvider,
    },
    subagent_content::{
        MAX_SUBAGENT_SOURCES_PER_CAPTURE, SubagentDiscovery,
        discover_claude_subagent_contents_bounded, discover_codex_subagent_contents_live,
        discover_codex_subagent_contents_stored,
    },
};

/// Optional asynchronous source location in the same registration as policy.
/// No runtime/store types cross this provider port.
#[async_trait]
pub(crate) trait LiveTranscriptSource: Send + Sync {
    async fn locate(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Option<Instant>,
    ) -> Result<Option<PathBuf>, LiveCandidateUnavailable>;

    async fn read_source(
        &self,
        source: std::fs::File,
        session_id: &str,
        cwd: &Path,
        cap: u64,
        deadline: Instant,
    ) -> crate::internal::ai::authorized_read::LiveClaudeSourceRead;
}

pub(crate) struct LiveCaptureRegistration<P: LiveCaptureProvider + ?Sized + 'static> {
    pub(crate) policy: &'static P,
    pub(crate) locator: Option<&'static dyn LiveTranscriptSource>,
}

/// Coverage-v1 normalizer over snapshot-owned redacted bytes (GC-ACF-03).
pub(crate) type LiveCoverageNormalizer = fn(&RedactedBytes) -> Vec<NormalizedTurn>;

/// Scope-verified facts the capture layer derives from a validated ingress
/// command. `verified_cwd` is the worktree directory bound at ingress, never
/// a provider-reported pointer.
#[derive(Clone, Copy)]
pub(crate) struct LiveCaptureContext<'a> {
    /// Libra's canonical `{hook_name}__{provider_session_id}` identity.
    pub(crate) libra_session_id: &'a str,
    pub(crate) provider_session_id: &'a str,
    pub(crate) verified_cwd: &'a Path,
}

/// A provider pointer is only an untrusted candidate, never read authority.
/// Deliberately neither Clone/Copy, Debug, nor Serialize: it has one transient
/// owner and must not be copied into generic lifecycle metadata or logging.
pub(crate) enum UntrustedChildSourceHints {
    Absent,
    Unavailable,
    Candidate(ChildSourceHint),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildSourceHintRole {
    CurrentThread,
    ChildBoundary,
}

pub(crate) struct ChildSourceHint {
    role: ChildSourceHintRole,
    current_or_parent_path: Option<PathBuf>,
    child_path: Option<PathBuf>,
    child_id: Option<String>,
    turn_id: Option<String>,
}

pub(crate) struct ChildSourceHintView<'a> {
    pub(crate) role: ChildSourceHintRole,
    pub(crate) current_or_parent_path: Option<&'a Path>,
    pub(crate) child_path: Option<&'a Path>,
    pub(crate) child_id: Option<&'a str>,
    pub(crate) turn_id: Option<&'a str>,
}

impl UntrustedChildSourceHints {
    pub(crate) fn candidate(&self) -> Option<ChildSourceHintView<'_>> {
        match self {
            Self::Candidate(hint) => Some(ChildSourceHintView {
                role: hint.role,
                current_or_parent_path: hint.current_or_parent_path.as_deref(),
                child_path: hint.child_path.as_deref(),
                child_id: hint.child_id.as_deref(),
                turn_id: hint.turn_id.as_deref(),
            }),
            Self::Absent | Self::Unavailable => None,
        }
    }

    fn bounded(&self) -> bool {
        let Some(hint) = self.candidate() else {
            return true;
        };
        let mut bytes = 0_usize;
        for path in [hint.current_or_parent_path, hint.child_path]
            .into_iter()
            .flatten()
        {
            let Some(path) = path.to_str() else {
                return false;
            };
            if path.trim().is_empty() || path.len() > 4096 || path.contains('\0') {
                return false;
            }
            bytes = bytes.saturating_add(path.len());
        }
        for id in [hint.child_id, hint.turn_id].into_iter().flatten() {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return false;
            }
            bytes = bytes.saturating_add(id.len());
        }
        bytes <= 8320
            && match hint.role {
                ChildSourceHintRole::CurrentThread => {
                    hint.child_path.is_none() && hint.child_id.is_none() && hint.turn_id.is_none()
                }
                ChildSourceHintRole::ChildBoundary => {
                    hint.child_path.is_some() && hint.child_id.is_some()
                }
            }
    }
}

/// The ingress view exposes only the pure extractor, not source locators,
/// exporters or discovery methods held by the already-selected binding.
#[derive(Clone, Copy)]
pub(crate) struct ChildSourceHintPolicy {
    hook_name: &'static str,
    kind: AgentKind,
    policy: &'static dyn LiveCaptureProvider,
}

impl ChildSourceHintPolicy {
    pub(crate) fn matches(self, provider: &dyn HookProvider) -> bool {
        self.hook_name == provider.provider_name() && self.kind == provider.agent_kind()
    }

    pub(crate) fn extract(
        self,
        envelope: &SessionHookEnvelope,
        kind: LifecycleEventKind,
    ) -> UntrustedChildSourceHints {
        let hints = self.policy.child_source_hints(envelope, kind);
        if hints.bounded() {
            hints
        } else {
            UntrustedChildSourceHints::Unavailable
        }
    }
}

/// The provider could not derive a live transcript candidate from the
/// verified facts. Content-free: the caller records only a fixed reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveCandidateUnavailable;

/// Pure lifecycle selection. Source IO and persistence remain in their owners.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubagentEventPolicy {
    ParentOnly,
    Boundary,
    RequiredTerminal,
}

/// Provider-owned capabilities default to neutral, source-absent behavior.
pub(crate) trait LiveCaptureProvider: Send + Sync {
    /// Pure completion compatibility; the id grants no source access or
    /// native provenance. Ordinary data discovery is a separate capability.
    fn subagent_event_policy(
        &self,
        _kind: LifecycleEventKind,
        _provider_session_id: &str,
    ) -> SubagentEventPolicy {
        SubagentEventPolicy::ParentOnly
    }
    /// Pure extraction of bounded untrusted candidates; no source IO or
    /// persistence. Providers without this policy return `Absent`.
    fn child_source_hints(
        &self,
        _envelope: &SessionHookEnvelope,
        _kind: LifecycleEventKind,
    ) -> UntrustedChildSourceHints {
        UntrustedChildSourceHints::Absent
    }
    /// Capture budget established at the AgentTraces boundary when the host
    /// passed no managed `--capture-budget-ms`. `None` keeps the legacy
    /// no-deadline behavior.
    fn missing_host_capture_budget(&self) -> Option<Duration> {
        None
    }

    /// Deterministic live transcript candidate derived from the verified cwd
    /// and native session id. Must not open, stat or read the candidate.
    fn live_transcript_candidate(
        &self,
        _context: &LiveCaptureContext<'_>,
    ) -> Result<Option<PathBuf>, LiveCandidateUnavailable> {
        Ok(None)
    }

    /// Coverage-v1 normalizer that gates an authorized live source per turn.
    /// `None` keeps the legacy ungated append.
    fn live_coverage_normalizer(&self) -> Option<LiveCoverageNormalizer> {
        None
    }

    /// Bounded child-transcript discovery for a committed parent checkpoint.
    fn subagent_discovery(&self) -> Option<&dyn LiveSubagentDiscovery> {
        None
    }

    /// Trusted transcript exporter for a provider without an on-disk live
    /// transcript. `Some` makes the capture layer serialize runs through its
    /// export-job runner lease before calling [`LiveTranscriptExporter::export`].
    fn transcript_exporter(&self) -> Option<&dyn LiveTranscriptExporter> {
        None
    }
}

/// Availability is independent of a bounded scan's partial data result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubagentDiscoveryAvailability {
    #[default]
    Available,
    Unavailable,
}

/// Explicit per-call stored discovery allowances. Providers may only reduce them.
#[derive(Clone, Copy)]
pub(crate) struct SubagentDiscoveryLimits {
    pub(crate) byte_budget: u64,
    pub(crate) source_limit: usize,
}

/// A transient untrusted locator is only a candidate. No Debug/Serialize or
/// second provider lookup is attached to this selection.
pub(crate) enum StoredSubagentSelection<'a> {
    ImportCurrent { parent_locator: Option<&'a Path> },
    TerminalRecoveryRoot,
}

/// Bounded discovery of the child transcripts attributable to one parent
/// session. The capture layer owns persistence and partial-result policy.
#[async_trait]
pub(crate) trait LiveSubagentDiscovery: Send + Sync {
    /// Opt in to coarse managed-parent partial metadata for unavailable or
    /// incomplete discovery. Existing integrations retain their metadata.
    fn marks_incomplete_parent_extraction(&self) -> bool {
        false
    }

    async fn discover(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<SubagentDiscovery>;

    async fn discover_with_hint(
        &self,
        context: &LiveCaptureContext<'_>,
        _hint: &UntrustedChildSourceHints,
        deadline: Instant,
    ) -> Result<SubagentDiscovery> {
        self.discover(context, deadline).await
    }

    /// Default is zero IO, never a delegate that silently drops caller caps.
    async fn discover_stored(
        &self,
        _context: &LiveCaptureContext<'_>,
        _selection: StoredSubagentSelection<'_>,
        _limits: SubagentDiscoveryLimits,
        _deadline: Instant,
    ) -> Result<SubagentDiscovery> {
        Ok(SubagentDiscovery {
            incomplete: true,
            availability: SubagentDiscoveryAvailability::Unavailable,
            warning: Some("stored subagent discovery is unavailable".to_string()),
            ..SubagentDiscovery::default()
        })
    }
}

/// Trusted export of one provider session's transcript (ADR-ACF-10, ACF-20).
/// The default is that the capture layer already holds the export-job runner
/// lease, and this trait never receives that lease.
///
/// Hermes is the exception: [`LiveTranscriptExporter::manages_export_lease`]
/// is true, so the capture layer calls `export` before admitting its own
/// runner. That call uses `hermes_export::read_hermes_live_source`, which
/// acquires the same export-job lease, runs the fixed argv, and releases it.
#[async_trait]
pub(crate) trait LiveTranscriptExporter: Send + Sync {
    /// When true, `export` acquires and releases the export-job lease itself.
    /// The capture layer must call it before admitting its own runner.
    fn manages_export_lease(&self) -> bool {
        false
    }

    /// Export the session transcript before `deadline`. A successful export
    /// is an authorized `TranscriptSource::Bytes`; an unavailable bridge
    /// reports its own content-free `reason=` before returning `Err`.
    async fn export(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<TranscriptSource>;

    /// Coverage-v1 normalizer over the exported transcript's redacted bytes.
    fn export_coverage_normalizer(&self) -> LiveCoverageNormalizer;
}

/// Product of the single provider lookup a hook callback performs. It is
/// `Copy` and `'static`, so the live pipeline can hand it to every stage
/// without reopening the provider.
#[derive(Clone, Copy)]
pub(crate) struct LiveCaptureBinding {
    hook_name: &'static str,
    kind: AgentKind,
    observed: &'static dyn ObservedAgent,
    live: &'static dyn LiveCaptureProvider,
    locator: Option<&'static dyn LiveTranscriptSource>,
}

impl LiveCaptureBinding {
    /// The runtime's only provider lookup: infallible, allocation-free and
    /// independent of configuration, so it adds no pre-ingress error path.
    pub(crate) fn resolve(provider: &dyn HookProvider) -> Self {
        Self::for_kind(provider.provider_name(), provider.agent_kind())
    }

    fn for_kind(hook_name: &'static str, kind: AgentKind) -> Self {
        let registration = live_capture_for(kind);
        Self {
            hook_name,
            kind,
            observed: selected_snapshot_adapter(kind),
            live: registration
                .map(|entry| entry.policy)
                .unwrap_or(&NEUTRAL_LIVE_CAPTURE),
            locator: registration.and_then(|entry| entry.locator),
        }
    }

    /// Reuse the original registered policy for stored callers. Their durable
    /// Libra/native identities are supplied explicitly, not rebuilt from this label.
    pub(crate) fn for_stored(kind: AgentKind) -> Self {
        Self::for_kind(agent_for(kind).provider_name(), kind)
    }

    /// The hook provider name: the session-id prefix and telemetry field.
    pub(crate) fn hook_name(self) -> &'static str {
        self.hook_name
    }

    /// The durable `agent_session.agent_kind` spelling of the typed identity.
    pub(crate) fn agent_kind_db(self) -> &'static str {
        self.kind.as_db_str()
    }

    pub(crate) fn child_source_hint_policy(self) -> ChildSourceHintPolicy {
        ChildSourceHintPolicy {
            hook_name: self.hook_name,
            kind: self.kind,
            policy: self.live,
        }
    }

    /// The observed-agent adapter used by the snapshot service.
    pub(crate) fn observed(self) -> &'static dyn ObservedAgent {
        self.observed
    }

    pub(crate) fn missing_host_capture_budget(self) -> Option<Duration> {
        self.live.missing_host_capture_budget()
    }

    pub(crate) fn live_transcript_candidate(
        self,
        context: &LiveCaptureContext<'_>,
    ) -> Result<Option<PathBuf>, LiveCandidateUnavailable> {
        self.live.live_transcript_candidate(context)
    }

    pub(crate) fn live_transcript_locator(self) -> Option<&'static dyn LiveTranscriptSource> {
        self.locator
    }

    pub(crate) fn live_coverage_normalizer(self) -> Option<LiveCoverageNormalizer> {
        self.live.live_coverage_normalizer()
    }

    pub(crate) fn subagent_discovery(self) -> Option<&'static dyn LiveSubagentDiscovery> {
        self.live.subagent_discovery()
    }

    pub(crate) fn subagent_event_policy(
        self,
        kind: LifecycleEventKind,
        provider_session_id: &str,
    ) -> SubagentEventPolicy {
        self.live.subagent_event_policy(kind, provider_session_id)
    }

    pub(crate) fn transcript_exporter(self) -> Option<&'static dyn LiveTranscriptExporter> {
        self.live.transcript_exporter()
    }
}

/// Preserve the single registered adapter selection. The scoped override is
/// compiled only for tests, allowing the actual public/import/child callers
/// to exercise enablement without changing any production registration.
pub(crate) fn selected_snapshot_adapter(kind: AgentKind) -> &'static dyn ObservedAgent {
    #[cfg(test)]
    if let Some(adapter) = TEST_SNAPSHOT_ADAPTER.with(|slot| {
        slot.borrow()
            .filter(|(selected, _)| *selected == kind)
            .map(|(_, adapter)| adapter)
    }) {
        return adapter;
    }
    agent_for(kind)
}

#[cfg(test)]
thread_local! {
    static TEST_SNAPSHOT_ADAPTER: std::cell::RefCell<Option<(AgentKind, &'static dyn ObservedAgent)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestSnapshotAdapterScope {
    previous: Option<(AgentKind, &'static dyn ObservedAgent)>,
    _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
impl Drop for TestSnapshotAdapterScope {
    fn drop(&mut self) {
        TEST_SNAPSHOT_ADAPTER.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}

#[cfg(test)]
pub(crate) fn scoped_test_snapshot_adapter(
    kind: AgentKind,
    adapter: &'static dyn ObservedAgent,
) -> TestSnapshotAdapterScope {
    assert_eq!(adapter.provider_kind(), kind);
    let previous = TEST_SNAPSHOT_ADAPTER.with(|slot| slot.replace(Some((kind, adapter))));
    TestSnapshotAdapterScope {
        previous,
        _same_thread: Default::default(),
    }
}

/// Source-absent defaults for every kind without a live capability.
struct NeutralLiveCapture;

static NEUTRAL_LIVE_CAPTURE: NeutralLiveCapture = NeutralLiveCapture;

impl LiveCaptureProvider for NeutralLiveCapture {}

/// Claude's native layout is deterministically scoped by the verified cwd and
/// provider session id, so it alone derives a live transcript candidate and
/// discovers child transcripts.
pub(super) struct ClaudeLiveCapture;

impl LiveCaptureProvider for ClaudeLiveCapture {
    fn live_transcript_candidate(
        &self,
        context: &LiveCaptureContext<'_>,
    ) -> Result<Option<PathBuf>, LiveCandidateUnavailable> {
        let candidate =
            claude_session_file_candidate(context.verified_cwd, context.provider_session_id)
                .map_err(|_| LiveCandidateUnavailable)?;
        match candidate {
            Some(path)
                if !claude_jsonl_session_matches(
                    &path.to_string_lossy(),
                    context.provider_session_id,
                ) =>
            {
                Ok(None)
            }
            other => Ok(other),
        }
    }

    fn live_coverage_normalizer(&self) -> Option<LiveCoverageNormalizer> {
        Some(claude_live_coverage)
    }

    fn subagent_discovery(&self) -> Option<&dyn LiveSubagentDiscovery> {
        Some(self)
    }
}

#[async_trait]
impl LiveSubagentDiscovery for ClaudeLiveCapture {
    async fn discover(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<SubagentDiscovery> {
        discover_claude_subagent_contents_bounded(
            context.verified_cwd,
            context.provider_session_id,
            deadline,
            TRANSCRIPT_READ_HARD_CAP_BYTES,
            MAX_SUBAGENT_SOURCES_PER_CAPTURE,
        )
        .await
    }
    async fn discover_stored(
        &self,
        context: &LiveCaptureContext<'_>,
        _selection: StoredSubagentSelection<'_>,
        limits: SubagentDiscoveryLimits,
        deadline: Instant,
    ) -> Result<SubagentDiscovery> {
        discover_claude_subagent_contents_bounded(
            context.verified_cwd,
            context.provider_session_id,
            deadline,
            limits.byte_budget,
            limits.source_limit,
        )
        .await
    }
}

fn claude_live_coverage(transcript: &RedactedBytes) -> Vec<NormalizedTurn> {
    let mut turns = normalize_claude_transcript(transcript.bytes());
    note_claude_attached_duplicates(&mut turns, transcript.bytes());
    turns
}

/// Codex keeps the frozen V1 rollout normalizer. The same registration's
/// optional source capability supplies the deadline-bound ordinary bytes.
pub(super) struct CodexLiveCapture;

// The pinned ThreadId decoder accepts every UUID version; shape selects
// live compatibility behavior, never source authority or provenance.
fn codex_live_child_session(session_id: &str) -> bool {
    uuid::Uuid::parse_str(session_id).is_ok()
}

impl LiveCaptureProvider for CodexLiveCapture {
    fn subagent_discovery(&self) -> Option<&dyn LiveSubagentDiscovery> {
        Some(self)
    }

    fn subagent_event_policy(
        &self,
        kind: LifecycleEventKind,
        provider_session_id: &str,
    ) -> SubagentEventPolicy {
        // The pinned Codex ThreadId decoder accepts UUIDs (new threads use
        // UUIDv7). Legacy raw labels retain their original parent-only live
        // route. Scoped historical import and recovery can still collect
        // their ordinary child data without this live qualification.
        if !codex_live_child_session(provider_session_id) {
            return SubagentEventPolicy::ParentOnly;
        }
        match kind {
            LifecycleEventKind::SubagentEnd => SubagentEventPolicy::Boundary,
            LifecycleEventKind::SessionEnd => SubagentEventPolicy::RequiredTerminal,
            _ => SubagentEventPolicy::ParentOnly,
        }
    }
    fn child_source_hints(
        &self,
        envelope: &SessionHookEnvelope,
        kind: LifecycleEventKind,
    ) -> UntrustedChildSourceHints {
        let role = match kind {
            LifecycleEventKind::TurnEnd | LifecycleEventKind::SessionEnd => {
                ChildSourceHintRole::CurrentThread
            }
            LifecycleEventKind::SubagentEnd => ChildSourceHintRole::ChildBoundary,
            _ => return UntrustedChildSourceHints::Absent,
        };
        let candidate = || -> Option<ChildSourceHint> {
            fn path(value: &str) -> Option<PathBuf> {
                (!value.trim().is_empty() && value.len() <= 4096 && !value.contains('\0'))
                    .then(|| PathBuf::from(value))
            }
            fn id(value: &str) -> Option<String> {
                (!value.is_empty()
                    && value.len() <= 64
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
                .then(|| value.to_owned())
            }
            let mut hint = ChildSourceHint {
                role,
                current_or_parent_path: match envelope.transcript_path.as_deref() {
                    Some(value) => Some(path(value)?),
                    None => None,
                },
                child_path: None,
                child_id: None,
                turn_id: None,
            };
            if role == ChildSourceHintRole::ChildBoundary {
                hint.child_path = Some(path(
                    envelope.extra.get("agent_transcript_path")?.as_str()?,
                )?);
                hint.child_id = Some(id(envelope.extra.get("agent_id")?.as_str()?)?);
                hint.turn_id = match envelope.extra.get("turn_id") {
                    Some(value) => Some(id(value.as_str()?)?),
                    None => None,
                };
            }
            Some(hint)
        };
        match candidate() {
            Some(hint) => {
                let hints = UntrustedChildSourceHints::Candidate(hint);
                if hints.bounded() {
                    hints
                } else {
                    UntrustedChildSourceHints::Unavailable
                }
            }
            None => UntrustedChildSourceHints::Unavailable,
        }
    }
    fn live_coverage_normalizer(&self) -> Option<LiveCoverageNormalizer> {
        Some(codex_live_coverage)
    }
}

// Live callbacks, explicit import, and terminal recovery select this same
// registered port. The shared Root driver owns reads and durable writes.
#[async_trait]
impl LiveSubagentDiscovery for CodexLiveCapture {
    fn marks_incomplete_parent_extraction(&self) -> bool {
        true
    }

    async fn discover(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<SubagentDiscovery> {
        self.discover_with_hint(context, &UntrustedChildSourceHints::Absent, deadline)
            .await
    }

    async fn discover_with_hint(
        &self,
        context: &LiveCaptureContext<'_>,
        hint: &UntrustedChildSourceHints,
        deadline: Instant,
    ) -> Result<SubagentDiscovery> {
        if !codex_live_child_session(context.provider_session_id) {
            return Ok(SubagentDiscovery::default());
        }
        discover_codex_subagent_contents_live(context, hint, deadline).await
    }

    async fn discover_stored(
        &self,
        context: &LiveCaptureContext<'_>,
        selection: StoredSubagentSelection<'_>,
        limits: SubagentDiscoveryLimits,
        deadline: Instant,
    ) -> Result<SubagentDiscovery> {
        discover_codex_subagent_contents_stored(context, selection, limits, deadline).await
    }
}

#[async_trait]
impl LiveTranscriptSource for CodexLiveCapture {
    async fn locate(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Option<Instant>,
    ) -> Result<Option<PathBuf>, LiveCandidateUnavailable> {
        super::codex_index::locate_live(context.provider_session_id, deadline).await
    }
    async fn read_source(
        &self,
        source: std::fs::File,
        session_id: &str,
        cwd: &Path,
        cap: u64,
        deadline: Instant,
    ) -> crate::internal::ai::authorized_read::LiveClaudeSourceRead {
        crate::internal::ai::authorized_read::read_live_codex_source_until(
            source, cap, session_id, cwd, deadline,
        )
        .await
    }
}

fn codex_live_coverage(transcript: &RedactedBytes) -> Vec<NormalizedTurn> {
    normalize_codex_rollout(transcript.bytes())
}

/// OpenCode's plugin supplies no managed capture budget, but its export
/// subprocess owns a bounded deadline: that budget becomes the one paired
/// capture deadline before any durable runtime work. OpenCode has no on-disk
/// transcript; its content arrives through the trusted, sandboxed
/// `opencode export` bridge (DR-04b).
pub(super) struct OpenCodeLiveCapture;

/// Grok is hook-installable, so the live lookup must be `Some`. It does not
/// derive a transcript candidate, export, or coverage normalizer here.
pub(super) struct GrokLiveCapture;

impl LiveCaptureProvider for GrokLiveCapture {}

/// PI-36: Pi is not hook-installable yet, but its observe path already needs
/// a bounded session source for automatic checkpoints before PI-06 promotes
/// it. The exporter returns the selected session bytes as an authorized
/// in-memory source, so the snapshot runs the PI-33 worker frame.
pub(super) struct PiLiveCapture;

impl LiveCaptureProvider for PiLiveCapture {
    fn missing_host_capture_budget(&self) -> Option<Duration> {
        // The extension passes a 1500 ms host budget. The same fixed value
        // keeps a directly invoked hook bounded: the source read or the
        // checkpoint write finishes (or fails closed) before PI-13's delivery
        // window can kill the child.
        Some(Duration::from_millis(1500))
    }

    fn transcript_exporter(&self) -> Option<&dyn LiveTranscriptExporter> {
        Some(self)
    }
}

#[async_trait]
impl LiveTranscriptExporter for PiLiveCapture {
    async fn export(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<TranscriptSource> {
        let Some(directory) = super::pi_live::default_pi_session_directory(context.verified_cwd)
        else {
            anyhow::bail!("pi default session directory is unavailable");
        };
        let read = super::pi_live::read_pi_session_source_until(
            &directory,
            context.provider_session_id,
            context.verified_cwd,
            super::builtin::pi::PiDiscoveryLimits::default(),
            deadline,
        )
        .await;
        let Some(bytes) = read.bytes() else {
            anyhow::bail!("pi session source is unavailable");
        };
        let auth = ExportAuthorized::issue("pi", context.libra_session_id, &bytes);
        Ok(TranscriptSource::Bytes { bytes, auth })
    }

    fn export_coverage_normalizer(&self) -> LiveCoverageNormalizer {
        pi_export_coverage
    }
}

fn pi_export_coverage(transcript: &RedactedBytes) -> Vec<NormalizedTurn> {
    super::builtin::pi::normalize_pi_turns(
        transcript.bytes(),
        super::builtin::pi::PiNormalizeLimits::default(),
    )
    .unwrap_or_default()
}

/// Hermes is hook-installable, so the live lookup must be `Some`. Thinking
/// text arrives through the JSONL export reader on the live snapshot path.
pub(super) struct HermesLiveCapture;

impl LiveCaptureProvider for HermesLiveCapture {
    fn missing_host_capture_budget(&self) -> Option<Duration> {
        // The subprocess itself stays inside `HERMES_LIVE_DEADLINE`. The
        // capture wrapper needs the same room as the other export bridge so
        // the checkpoint write is not cancelled after a fast export.
        Some(super::opencode_export::ExportLimits::default().deadline)
    }

    fn transcript_exporter(&self) -> Option<&dyn LiveTranscriptExporter> {
        Some(self)
    }
}

#[async_trait]
impl LiveTranscriptExporter for HermesLiveCapture {
    fn manages_export_lease(&self) -> bool {
        true
    }

    async fn export(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<TranscriptSource> {
        let read = super::hermes_export::export_for_live_capture(
            context.provider_session_id,
            context.verified_cwd,
            deadline,
        )
        .await?;
        if read.readable_thinking.is_empty() || read.transcript.is_empty() {
            anyhow::bail!("hermes export bridge unavailable");
        }
        Ok(TranscriptSource::from_hermes_projection(
            context.libra_session_id,
            read.transcript,
        ))
    }

    fn export_coverage_normalizer(&self) -> LiveCoverageNormalizer {
        hermes_export_coverage
    }
}

fn hermes_export_coverage(transcript: &RedactedBytes) -> Vec<NormalizedTurn> {
    // Ordinary turns come from the selected export. Thinking stays in the
    // retained JSONL and is not copied into the turn prompt.
    match super::builtin::hermes::hermes_worker_frame_authorized(transcript.bytes(), None) {
        Ok(frame) => frame.ordinary,
        Err(_) => Vec::new(),
    }
}

impl LiveCaptureProvider for OpenCodeLiveCapture {
    fn missing_host_capture_budget(&self) -> Option<Duration> {
        Some(ExportLimits::default().deadline)
    }

    fn transcript_exporter(&self) -> Option<&dyn LiveTranscriptExporter> {
        Some(self)
    }
}

#[async_trait]
impl LiveTranscriptExporter for OpenCodeLiveCapture {
    async fn export(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<TranscriptSource> {
        let exported = authorized_trusted_sandboxed_export_until(
            context.provider_session_id,
            context.libra_session_id,
            context.verified_cwd,
            ExportLimits::default(),
            deadline,
        )
        .await;
        if exported.is_err() {
            // Bridge unavailability (untrusted binary, no sandbox) degrades
            // to a metadata-only capture; the structured reason is fixed and
            // no exporter diagnostic is rendered. The rendered chain stays
            // opt-in at debug level so a host or containment failure remains
            // diagnosable without putting free text into hook stderr.
            if let Err(error) = &exported {
                tracing::debug!(
                    error = %format!("{error:#}"),
                    "opencode export bridge failure detail"
                );
            }
            tracing::warn!(
                reason = "opencode_export_bridge_unavailable",
                "opencode export bridge unavailable; metadata-only capture"
            );
        }
        exported
    }

    fn export_coverage_normalizer(&self) -> LiveCoverageNormalizer {
        opencode_export_coverage
    }
}

fn opencode_export_coverage(transcript: &RedactedBytes) -> Vec<NormalizedTurn> {
    normalize_opencode_export(transcript.bytes())
}

/// Test seam: a binding whose live capability delegates to the resolved one
/// but answers [`LiveCaptureProvider::transcript_exporter`] with a scripted
/// exporter, so capture tests can drive every export exit without a trusted
/// bridge. It lives here because every `LiveCaptureProvider` implementation
/// belongs to this provider layer.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) mod test_support {
    use super::*;

    struct ScriptedExportCapture {
        base: &'static dyn LiveCaptureProvider,
        exporter: &'static dyn LiveTranscriptExporter,
    }

    impl LiveCaptureProvider for ScriptedExportCapture {
        fn subagent_event_policy(
            &self,
            kind: LifecycleEventKind,
            provider_session_id: &str,
        ) -> SubagentEventPolicy {
            self.base.subagent_event_policy(kind, provider_session_id)
        }
        fn child_source_hints(
            &self,
            envelope: &SessionHookEnvelope,
            kind: LifecycleEventKind,
        ) -> UntrustedChildSourceHints {
            self.base.child_source_hints(envelope, kind)
        }
        fn missing_host_capture_budget(&self) -> Option<Duration> {
            self.base.missing_host_capture_budget()
        }

        fn live_transcript_candidate(
            &self,
            context: &LiveCaptureContext<'_>,
        ) -> Result<Option<PathBuf>, LiveCandidateUnavailable> {
            self.base.live_transcript_candidate(context)
        }

        fn live_coverage_normalizer(&self) -> Option<LiveCoverageNormalizer> {
            self.base.live_coverage_normalizer()
        }

        fn subagent_discovery(&self) -> Option<&dyn LiveSubagentDiscovery> {
            self.base.subagent_discovery()
        }

        fn transcript_exporter(&self) -> Option<&dyn LiveTranscriptExporter> {
            Some(self.exporter)
        }
    }

    #[cfg(feature = "test-live-agent")]
    struct ScriptedDiscoveryCapture {
        base: &'static dyn LiveCaptureProvider,
        discovery: &'static dyn LiveSubagentDiscovery,
    }

    #[cfg(feature = "test-live-agent")]
    impl LiveCaptureProvider for ScriptedDiscoveryCapture {
        fn subagent_event_policy(
            &self,
            kind: LifecycleEventKind,
            provider_session_id: &str,
        ) -> SubagentEventPolicy {
            self.base.subagent_event_policy(kind, provider_session_id)
        }
        fn child_source_hints(
            &self,
            envelope: &SessionHookEnvelope,
            kind: LifecycleEventKind,
        ) -> UntrustedChildSourceHints {
            self.base.child_source_hints(envelope, kind)
        }
        fn missing_host_capture_budget(&self) -> Option<Duration> {
            self.base.missing_host_capture_budget()
        }
        fn live_transcript_candidate(
            &self,
            context: &LiveCaptureContext<'_>,
        ) -> Result<Option<PathBuf>, LiveCandidateUnavailable> {
            self.base.live_transcript_candidate(context)
        }
        fn live_coverage_normalizer(&self) -> Option<LiveCoverageNormalizer> {
            self.base.live_coverage_normalizer()
        }
        fn subagent_discovery(&self) -> Option<&dyn LiveSubagentDiscovery> {
            Some(self.discovery)
        }
        fn transcript_exporter(&self) -> Option<&dyn LiveTranscriptExporter> {
            self.base.transcript_exporter()
        }
    }

    #[cfg(feature = "test-live-agent")]
    pub(crate) fn with_subagent_discovery(
        binding: LiveCaptureBinding,
        discovery: &'static dyn LiveSubagentDiscovery,
    ) -> LiveCaptureBinding {
        let live = Box::leak(Box::new(ScriptedDiscoveryCapture {
            base: binding.live,
            discovery,
        }));
        LiveCaptureBinding { live, ..binding }
    }

    #[cfg(feature = "test-live-agent")]
    type BeforeChildDiscovery =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>;

    #[cfg(feature = "test-live-agent")]
    #[derive(Clone)]
    pub(crate) struct StoredDiscoveryCall {
        pub(crate) libra_session_id: String,
        pub(crate) provider_session_id: String,
        pub(crate) cwd: PathBuf,
        pub(crate) byte_budget: u64,
        pub(crate) source_limit: usize,
        pub(crate) deadline: Instant,
        pub(crate) recovery_root: bool,
    }

    /// Local-only typed-port script. Source fixtures and any database fault
    /// remain owned by the test caller, outside the provider layer.
    #[cfg(feature = "test-live-agent")]
    #[derive(Default)]
    pub(crate) struct ScriptedChildDiscovery {
        result: std::sync::Mutex<SubagentDiscovery>,
        calls: std::sync::atomic::AtomicUsize,
        before: std::sync::Mutex<Option<BeforeChildDiscovery>>,
        stored_calls: std::sync::Mutex<Vec<StoredDiscoveryCall>>,
    }

    #[cfg(feature = "test-live-agent")]
    impl ScriptedChildDiscovery {
        pub(crate) fn set_result(&self, result: SubagentDiscovery) {
            *self.result.lock().expect("scripted discovery result") = result;
        }

        pub(crate) fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        pub(crate) fn stored_calls(&self) -> Vec<StoredDiscoveryCall> {
            self.stored_calls.lock().expect("stored calls").clone()
        }

        pub(crate) fn before_next(&self, future: BeforeChildDiscovery) {
            *self.before.lock().expect("scripted discovery pause") = Some(future);
        }
    }

    #[cfg(feature = "test-live-agent")]
    #[async_trait::async_trait]
    impl LiveSubagentDiscovery for ScriptedChildDiscovery {
        async fn discover(
            &self,
            _: &LiveCaptureContext<'_>,
            _: Instant,
        ) -> Result<SubagentDiscovery> {
            panic!("scripted child discovery requires the typed hint")
        }

        async fn discover_with_hint(
            &self,
            _: &LiveCaptureContext<'_>,
            hint: &UntrustedChildSourceHints,
            deadline: Instant,
        ) -> Result<SubagentDiscovery> {
            anyhow::ensure!(
                Instant::now() < deadline,
                "scripted discovery deadline expired"
            );
            assert!(
                hint.candidate().is_some(),
                "typed candidate must reach the port"
            );
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let before = self.before.lock().expect("scripted discovery pause").take();
            if let Some(before) = before {
                before.await?;
            }
            Ok(self
                .result
                .lock()
                .expect("scripted discovery result")
                .clone())
        }
        async fn discover_stored(
            &self,
            context: &LiveCaptureContext<'_>,
            selection: StoredSubagentSelection<'_>,
            limits: SubagentDiscoveryLimits,
            deadline: Instant,
        ) -> Result<SubagentDiscovery> {
            anyhow::ensure!(Instant::now() < deadline, "stored fixture deadline");
            self.stored_calls
                .lock()
                .expect("stored calls")
                .push(StoredDiscoveryCall {
                    libra_session_id: context.libra_session_id.to_owned(),
                    provider_session_id: context.provider_session_id.to_owned(),
                    cwd: context.verified_cwd.to_path_buf(),
                    byte_budget: limits.byte_budget,
                    source_limit: limits.source_limit,
                    deadline,
                    recovery_root: matches!(
                        selection,
                        StoredSubagentSelection::TerminalRecoveryRoot
                    ),
                });
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let before = self.before.lock().expect("stored fixture pause").take();
            if let Some(before) = before {
                before.await?;
            }
            Ok(self.result.lock().expect("stored fixture result").clone())
        }
    }

    #[cfg(feature = "test-live-agent")]
    pub(crate) mod child_hint_recording {
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };

        use super::*;
        #[derive(Default)]
        pub(crate) struct RecordingDiscovery {
            pub(crate) calls: Mutex<Vec<(bool, Option<String>)>>,
        }
        #[async_trait::async_trait]
        impl LiveSubagentDiscovery for RecordingDiscovery {
            async fn discover(
                &self,
                _: &LiveCaptureContext<'_>,
                _: Instant,
            ) -> Result<SubagentDiscovery> {
                panic!("typed delegate must receive the hint");
            }
            async fn discover_with_hint(
                &self,
                _: &LiveCaptureContext<'_>,
                hint: &UntrustedChildSourceHints,
                _: Instant,
            ) -> Result<SubagentDiscovery> {
                let view = hint.candidate().expect("real policy candidate");
                self.calls.lock().expect("recorder").push((
                    view.role == ChildSourceHintRole::CurrentThread,
                    view.current_or_parent_path
                        .map(|p| p.to_string_lossy().into_owned()),
                ));
                Ok(SubagentDiscovery::default())
            }
        }
        #[derive(Default)]
        pub(crate) struct LegacyDiscovery(AtomicUsize);
        impl LegacyDiscovery {
            pub(crate) fn calls(&self) -> usize {
                self.0.load(Ordering::SeqCst)
            }
        }
        #[async_trait::async_trait]
        impl LiveSubagentDiscovery for LegacyDiscovery {
            async fn discover(
                &self,
                _: &LiveCaptureContext<'_>,
                _: Instant,
            ) -> Result<SubagentDiscovery> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(SubagentDiscovery {
                    warning: Some("legacy delegate".into()),
                    ..SubagentDiscovery::default()
                })
            }
        }
    }

    /// `binding` with its transcript exporter replaced by `exporter`. The
    /// small wrapper is leaked so the binding stays `Copy + 'static`.
    pub(crate) fn with_transcript_exporter(
        binding: LiveCaptureBinding,
        exporter: &'static dyn LiveTranscriptExporter,
    ) -> LiveCaptureBinding {
        let live: &'static ScriptedExportCapture = Box::leak(Box::new(ScriptedExportCapture {
            base: binding.live,
            exporter,
        }));
        LiveCaptureBinding { live, ..binding }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ai::{
        hooks::providers::{claude_provider, codex_provider, gemini_provider, opencode_provider},
        observed_agents::{claude_project_slug, registration_for},
    };

    /// The pre-ACF-18 runtime's provider-name -> `agent_kind` bridge, kept
    /// verbatim as the legacy oracle for the typed identity.
    fn legacy_capture_agent_kind(provider_name: &str) -> &str {
        match provider_name {
            "claude" => "claude_code",
            "gemini" => "gemini",
            other => other,
        }
    }

    const CLAUDE_FIXTURE: &str = concat!(
        r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"run grep"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
        "\n",
        r#"{"type":"user","uuid":"u2","message":{"role":"user","content":"thanks"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a2","message":{"role":"assistant","content":[{"type":"text","text":"any time"}]}}"#,
    );

    const CODEX_FIXTURE: &str = concat!(
        r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}"#,
    );

    const OPENCODE_FIXTURE: &str = r#"{"info":{"id":"ses_fixture"},"messages":[{"info":{"id":"msg_u1","role":"user"},"parts":[{"type":"text","text":"hello"}]},{"info":{"id":"msg_a1","role":"assistant"},"parts":[{"type":"text","text":"hi"}]}]}"#;

    /// ADR-ACF-10 "Typed identity": every builtin hook provider resolves to
    /// the durable `agent_kind` the legacy name bridge produced, keeps its
    /// hook name as the session-id prefix, and agrees with the observed-agent
    /// registry's own hook mapping.
    #[test]
    fn binding_preserves_legacy_agent_kind_table() {
        let providers: [(&dyn HookProvider, &str, &str); 4] = [
            (claude_provider(), "claude", "claude_code"),
            (codex_provider(), "codex", "codex"),
            (gemini_provider(), "gemini", "gemini"),
            (opencode_provider(), "opencode", "opencode"),
        ];
        for (provider, hook_name, agent_kind) in providers {
            let binding = LiveCaptureBinding::resolve(provider);
            assert_eq!(binding.hook_name(), hook_name);
            assert_eq!(binding.hook_name(), provider.provider_name());
            assert_eq!(
                binding.agent_kind_db(),
                legacy_capture_agent_kind(provider.provider_name()),
                "{hook_name}: typed identity must match the legacy name bridge"
            );
            assert_eq!(binding.agent_kind_db(), agent_kind);
            assert_eq!(
                binding.observed().provider_kind(),
                provider.agent_kind(),
                "{hook_name}: the snapshot adapter must be the typed kind's adapter"
            );
            assert_eq!(
                AgentKind::from_db_str(binding.agent_kind_db()),
                Some(provider.agent_kind()),
                "{hook_name}: the durable spelling must round-trip to the typed kind"
            );
            if let Some(registered) = agent_for(provider.agent_kind()).as_hooks() {
                assert_eq!(
                    registered.provider_name(),
                    provider.provider_name(),
                    "{hook_name}: typed identity must agree with the registry's hook provider"
                );
            }
        }
    }

    /// Points `LIBRA_TEST_HOME` at a directory this test owns and restores
    /// the prior value on drop, even when an assertion unwinds.
    struct OwnedProviderHome {
        prior: Option<std::ffi::OsString>,
    }

    impl OwnedProviderHome {
        fn set(path: &Path) -> Self {
            let prior = std::env::var_os("LIBRA_TEST_HOME");
            // SAFETY: test-only process environment mutation, restored by
            // Drop; the test holds the serial env lane.
            unsafe { std::env::set_var("LIBRA_TEST_HOME", path) };
            Self { prior }
        }
    }

    impl Drop for OwnedProviderHome {
        fn drop(&mut self) {
            // SAFETY: paired with `set` under the same serial env lane.
            unsafe {
                match &self.prior {
                    Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }

    /// Every kind's budget, transcript candidate, coverage normalizer, child
    /// discovery and transcript exporter equal the pre-ACF-17 runtime's
    /// string-keyed decisions, and the lookup exists exactly for the
    /// hook-installable kinds. The Claude provider root comes from a home
    /// this test owns (under the serial env lane), so both derivations
    /// resolve under the same root and compare exactly.
    #[test]
    #[serial_test::serial(env)]
    fn live_capture_ports_match_legacy_runtime_decisions() {
        let home = tempfile::tempdir().expect("create owned provider home");
        let _home = OwnedProviderHome::set(home.path());
        let cwd = Path::new("/workspace/live-capture-fixture");
        let safe_session = "1f0c2a4e-live-capture";
        let claude_fixture = RedactedBytes::new_unchecked(CLAUDE_FIXTURE.as_bytes().to_vec());
        let codex_fixture = RedactedBytes::new_unchecked(CODEX_FIXTURE.as_bytes().to_vec());
        assert_eq!(normalize_claude_transcript(claude_fixture.bytes()).len(), 2);
        assert_eq!(normalize_codex_rollout(codex_fixture.bytes()).len(), 1);

        for kind in AgentKind::all() {
            let kind = *kind;
            let db = kind.as_db_str();
            assert_eq!(
                live_capture_for(kind).is_some(),
                registration_for(kind).hook_installable,
                "{db}: live capture must exist exactly for hook-installable kinds"
            );
            let binding = LiveCaptureBinding::for_kind(db, kind);

            let legacy_budget = match db {
                "opencode" | "hermes" => Some(ExportLimits::default().deadline),
                "pi" => Some(Duration::from_millis(1500)),
                _ => None,
            };
            assert_eq!(binding.missing_host_capture_budget(), legacy_budget, "{db}");

            // write_committed_checkpoint: `agent_kind == "claude_code"`
            // derived the candidate; every other kind stayed source-absent.
            let context = LiveCaptureContext {
                libra_session_id: "fixture__session",
                provider_session_id: safe_session,
                verified_cwd: cwd,
            };
            let candidate = binding.live_transcript_candidate(&context);
            let unsafe_context = LiveCaptureContext {
                provider_session_id: "../escape",
                ..context
            };
            let unsafe_candidate = binding.live_transcript_candidate(&unsafe_context);
            if db == "claude_code" {
                let expected = home
                    .path()
                    .join(".claude")
                    .join("projects")
                    .join(claude_project_slug(cwd))
                    .join(format!("{safe_session}.jsonl"));
                let legacy = claude_session_file_candidate(cwd, safe_session)
                    .expect("legacy derivation accepts a safe session id");
                assert_eq!(legacy.as_ref(), Some(&expected), "{db}");
                assert_eq!(candidate, Ok(legacy), "{db}");
                assert!(claude_session_file_candidate(cwd, "../escape").is_err());
                assert_eq!(unsafe_candidate, Err(LiveCandidateUnavailable));
            } else {
                assert_eq!(candidate, Ok(None), "{db}");
                assert_eq!(unsafe_candidate, Ok(None), "{db}");
            }

            // `matches!(agent_kind, "claude_code" | "codex")`, then the
            // Claude transcript or Codex rollout normalizer.
            match binding.live_coverage_normalizer() {
                Some(normalize) => {
                    let legacy: fn(&[u8]) -> Vec<NormalizedTurn> = match db {
                        "claude_code" => normalize_claude_transcript,
                        "codex" => normalize_codex_rollout,
                        other => panic!("{other}: unexpected live coverage normalizer"),
                    };
                    for fixture in [&claude_fixture, &codex_fixture] {
                        assert_eq!(normalize(fixture), legacy(fixture.bytes()), "{db}");
                    }
                }
                None => assert!(!matches!(db, "claude_code" | "codex"), "{db}"),
            }

            // Provider-neutral inventory; Codex functional tests stay local.
            assert_eq!(
                binding.subagent_discovery().is_some(),
                matches!(db, "claude_code" | "codex"),
                "{db}"
            );

            match binding.transcript_exporter() {
                Some(exporter) => {
                    assert!(
                        matches!(db, "opencode" | "hermes" | "pi"),
                        "{db}: unexpected transcript exporter"
                    );
                    let normalize = exporter.export_coverage_normalizer();
                    if db == "pi" {
                        let fixture = RedactedBytes::new_unchecked(Vec::new());
                        assert!(normalize(&fixture).is_empty(), "{db}");
                        assert!(!exporter.manages_export_lease(), "{db}");
                    } else if db == "opencode" {
                        let fixture =
                            RedactedBytes::new_unchecked(OPENCODE_FIXTURE.as_bytes().to_vec());
                        assert_eq!(normalize(&fixture).len(), 1, "{db}");
                        assert_eq!(
                            normalize(&fixture),
                            normalize_opencode_export(fixture.bytes()),
                            "{db}"
                        );
                    } else {
                        assert!(
                            exporter.manages_export_lease(),
                            "{db}: hermes export must take the export-job lease itself"
                        );
                        let fixture = RedactedBytes::new_unchecked(Vec::new());
                        assert!(normalize(&fixture).is_empty(), "{db}");
                    }
                }
                None => assert!(
                    !matches!(db, "opencode" | "hermes" | "pi"),
                    "{db}: missing transcript exporter"
                ),
            }
            // The live checkpoint writer runs live coverage and the export
            // stage as alternatives; one provider must never expose both.
            assert!(
                !(binding.live_coverage_normalizer().is_some()
                    && binding.transcript_exporter().is_some()),
                "{db}: a provider must not combine live coverage with a transcript exporter"
            );
        }
    }
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod codex_child_hint_local_tests {
    use super::*;
    use crate::internal::ai::hooks::providers::codex_provider;

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_stored_default_never_calls_legacy_unbounded_discovery() {
        let port = test_support::child_hint_recording::LegacyDiscovery::default();
        let context = LiveCaptureContext {
            libra_session_id: "stored-capture",
            provider_session_id: "stored-native",
            verified_cwd: Path::new("relative-untrusted-cwd"),
        };
        for selection in [
            StoredSubagentSelection::ImportCurrent {
                parent_locator: Some(Path::new("relative/../untrusted")),
            },
            StoredSubagentSelection::TerminalRecoveryRoot,
        ] {
            let result = port
                .discover_stored(
                    &context,
                    selection,
                    SubagentDiscoveryLimits {
                        byte_budget: 0,
                        source_limit: 0,
                    },
                    Instant::now() - std::time::Duration::from_secs(1),
                )
                .await
                .expect("default must perform zero IO even without a usable budget");
            assert_eq!(port.calls(), 0);
            assert_eq!(result.bytes_read, 0);
            assert!(result.sources.is_empty());
            assert!(result.incomplete);
            assert_eq!(
                result.warning.as_deref(),
                Some("stored subagent discovery is unavailable")
            );
        }
    }

    fn binding() -> LiveCaptureBinding {
        LiveCaptureBinding::resolve(codex_provider())
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_hint_native_terminal_policy_keeps_legacy_parent_only() {
        let binding = binding();
        for id in [
            "123e4567-e89b-12d3-a456-426614174031",
            "01890f82-182c-4f77-9934-6255f06e8739",
            "0199f145-9a2f-7847-a6f7-123456789abc",
            "00000000-0000-0000-0000-000000000000",
            "0199F145-9A2F-7847-A6F7-123456789ABC",
            "0199f1459a2f7847a6f7123456789abc",
            "{0199f145-9a2f-7847-a6f7-123456789abc}",
            "urn:uuid:0199f145-9a2f-7847-a6f7-123456789abc",
        ] {
            assert!(matches!(
                binding.subagent_event_policy(LifecycleEventKind::SessionEnd, id),
                SubagentEventPolicy::RequiredTerminal
            ));
            assert!(matches!(
                binding.subagent_event_policy(LifecycleEventKind::SubagentEnd, id),
                SubagentEventPolicy::Boundary
            ));
            assert!(matches!(
                binding.subagent_event_policy(LifecycleEventKind::TurnEnd, id),
                SubagentEventPolicy::ParentOnly
            ));
        }
        for id in [
            "sess-terminal-consumer-contract",
            "sess-codex-terminal-database-lock",
            "",
            "not-a-uuid",
            " 0199f145-9a2f-7847-a6f7-123456789abc",
            "0199f145-9a2f-7847-a6f7-123456789abc ",
        ] {
            for event in [
                LifecycleEventKind::SessionEnd,
                LifecycleEventKind::SubagentEnd,
            ] {
                assert!(matches!(
                    binding.subagent_event_policy(event, id),
                    SubagentEventPolicy::ParentOnly
                ));
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_hint_incomplete_parent_optin_preserves_existing_providers() {
        assert!(
            binding()
                .subagent_discovery()
                .unwrap()
                .marks_incomplete_parent_extraction()
        );
        let claude =
            LiveCaptureBinding::resolve(crate::internal::ai::hooks::providers::claude_provider());
        assert!(
            !claude
                .subagent_discovery()
                .unwrap()
                .marks_incomplete_parent_extraction()
        );
    }

    fn payload(event: &str, fields: serde_json::Value) -> Vec<u8> {
        let mut value = serde_json::json!({
            "hook_event_name":event,"session_id":"root-id","cwd":"/in-process-capture",
            "transcript_path":"/provider/parent.jsonl"
        });
        value
            .as_object_mut()
            .expect("fixture object")
            .extend(fields.as_object().expect("fixture fields").clone());
        serde_json::to_vec(&value).expect("fixture encoding")
    }
    fn hints(fields: serde_json::Value) -> UntrustedChildSourceHints {
        let envelope: SessionHookEnvelope =
            serde_json::from_slice(&payload("SubagentStop", fields)).expect("fixture envelope");
        binding()
            .child_source_hint_policy()
            .extract(&envelope, LifecycleEventKind::SubagentEnd)
    }
    fn fields() -> serde_json::Value {
        serde_json::json!({"agent_id":"child-id","agent_transcript_path":"/provider/child.jsonl","turn_id":"turn-id"})
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_hint_requires_distinct_child_fields_and_string_scalars() {
        for missing in ["agent_id", "agent_transcript_path"] {
            let mut f = fields();
            f.as_object_mut().expect("fields").remove(missing);
            assert!(matches!(hints(f), UntrustedChildSourceHints::Unavailable));
        }
        for key in ["agent_id", "agent_transcript_path", "turn_id"] {
            for value in [
                serde_json::json!(12),
                serde_json::json!(true),
                serde_json::json!({}),
                serde_json::Value::Null,
            ] {
                let mut f = fields();
                f[key] = value;
                assert!(matches!(hints(f), UntrustedChildSourceHints::Unavailable));
            }
        }
        let mut f = fields();
        f.as_object_mut().expect("fields").remove("turn_id");
        let h = hints(f);
        assert!(h.candidate().expect("turn optional").turn_id.is_none());
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_hint_bounds_do_not_normalize_or_promote_candidate_paths() {
        for (key, value) in [
            ("agent_id", "x".repeat(65)),
            ("turn_id", "bad/identifier".to_owned()),
            ("agent_transcript_path", "x".repeat(4097)),
            ("agent_transcript_path", "bad\0path".to_owned()),
            ("agent_transcript_path", " ".to_owned()),
        ] {
            let mut f = fields();
            f[key] = serde_json::Value::String(value);
            assert!(matches!(hints(f), UntrustedChildSourceHints::Unavailable));
        }
        let mut f = fields();
        f["agent_transcript_path"] = serde_json::json!("../candidate");
        let h = hints(f);
        assert_eq!(
            h.candidate().expect("candidate, not authority").child_path,
            Some(Path::new("../candidate"))
        );
        let mut envelope: SessionHookEnvelope =
            serde_json::from_slice(&payload("SubagentStop", fields())).expect("envelope");
        envelope.transcript_path = None;
        let hint = binding()
            .child_source_hint_policy()
            .extract(&envelope, LifecycleEventKind::SubagentEnd);
        assert!(
            hint.candidate()
                .expect("parent locator optional")
                .current_or_parent_path
                .is_none()
        );
        envelope.transcript_path = Some("p".repeat(4096));
        envelope.extra.insert(
            "agent_transcript_path".into(),
            serde_json::json!("c".repeat(4096)),
        );
        envelope
            .extra
            .insert("agent_id".into(), serde_json::json!("a".repeat(64)));
        envelope
            .extra
            .insert("turn_id".into(), serde_json::json!("t".repeat(64)));
        assert!(
            binding()
                .child_source_hint_policy()
                .extract(&envelope, LifecycleEventKind::SubagentEnd)
                .candidate()
                .is_some(),
            "exact 8320-byte bound must remain usable"
        );
        envelope.transcript_path = Some("p".repeat(4097));
        assert!(matches!(
            binding()
                .child_source_hint_policy()
                .extract(&envelope, LifecycleEventKind::SubagentEnd),
            UntrustedChildSourceHints::Unavailable
        ));
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_hint_parent_events_ignore_unrelated_child_fields() {
        for kind in [LifecycleEventKind::TurnEnd, LifecycleEventKind::SessionEnd] {
            let envelope:SessionHookEnvelope=serde_json::from_slice(&payload("Stop",serde_json::json!({
                "agent_id": {"PRIVATE_UNKNOWN":"discard"}, "agent_transcript_path":42, "turn_id":false
            }))).expect("envelope");
            let h = binding()
                .child_source_hint_policy()
                .extract(&envelope, kind);
            let view = h.candidate().expect("current thread candidate");
            assert!(view.role == ChildSourceHintRole::CurrentThread);
            assert_eq!(
                view.current_or_parent_path,
                Some(Path::new("/provider/parent.jsonl"))
            );
            assert!(view.child_id.is_none() && view.child_path.is_none() && view.turn_id.is_none());
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_hint_other_kinds_and_providers_remain_neutral() {
        let envelope: SessionHookEnvelope =
            serde_json::from_slice(&payload("SubagentStop", fields())).expect("envelope");
        for kind in [
            LifecycleEventKind::SessionStart,
            LifecycleEventKind::TurnStart,
            LifecycleEventKind::ToolUse,
            LifecycleEventKind::ModelUpdate,
            LifecycleEventKind::Compaction,
            LifecycleEventKind::CompactionCompleted,
            LifecycleEventKind::PermissionRequest,
            LifecycleEventKind::SourceEnabled,
            LifecycleEventKind::SourceDisabled,
            LifecycleEventKind::SubagentStart,
            LifecycleEventKind::TurnInterrupted,
        ] {
            assert!(matches!(
                binding()
                    .child_source_hint_policy()
                    .extract(&envelope, kind),
                UntrustedChildSourceHints::Absent
            ));
        }
        let mut others = 0;
        for kind in AgentKind::all()
            .iter()
            .copied()
            .filter(|kind| *kind != AgentKind::Codex)
        {
            let policy =
                LiveCaptureBinding::for_kind(kind.as_db_str(), kind).child_source_hint_policy();
            assert!(matches!(
                policy.extract(&envelope, LifecycleEventKind::SubagentEnd),
                UntrustedChildSourceHints::Absent
            ));
            others += 1;
        }
        assert_eq!(others, 7);
    }
}
