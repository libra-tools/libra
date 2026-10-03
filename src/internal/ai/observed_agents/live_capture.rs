//! Live-capture provider capability (ADR-ACF-10, ACF-18/ACF-20).
//!
//! The shared live pipeline (`capture::live_pipeline`) and checkpoint writers
//! (`capture::live_checkpoint`) stay provider-neutral: every provider-specific
//! live decision -- the capture budget used when the host supplied none, the
//! deterministic transcript candidate, the live coverage normalizer,
//! child-transcript discovery and the trusted transcript exporter -- is
//! answered here through [`LiveCaptureProvider`]. Implementations receive only
//! scope-verified facts ([`LiveCaptureContext`]) and a monotonic deadline.
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
    AgentKind, NormalizedTurn, ObservedAgent, RedactedBytes, TRANSCRIPT_READ_HARD_CAP_BYTES,
    TranscriptSource, agent_for, claude_session_file_candidate, live_capture_for,
    normalize_claude_transcript, normalize_codex_rollout, normalize_opencode_export,
    opencode_export::{ExportLimits, authorized_trusted_sandboxed_export_until},
};
use crate::internal::ai::{
    hooks::provider::HookProvider,
    subagent_content::{
        MAX_SUBAGENT_SOURCES_PER_CAPTURE, SubagentDiscovery,
        discover_claude_subagent_contents_bounded,
    },
};

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

/// The provider could not derive a live transcript candidate from the
/// verified facts. Content-free: the caller records only a fixed reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveCandidateUnavailable;

/// Provider-owned live capture policy. Every method defaults to the neutral,
/// source-absent behavior; a provider overrides only what it supports.
pub(crate) trait LiveCaptureProvider: Send + Sync {
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

/// Bounded discovery of the child transcripts attributable to one parent
/// session. The capture layer owns persistence and partial-result policy.
#[async_trait]
pub(crate) trait LiveSubagentDiscovery: Send + Sync {
    async fn discover(
        &self,
        context: &LiveCaptureContext<'_>,
        deadline: Instant,
    ) -> Result<SubagentDiscovery>;
}

/// Trusted export of one provider session's transcript (ADR-ACF-10, ACF-20).
/// It is called only while the capture layer holds the export-job runner
/// lease, and it never receives the lease, a store or a capture scope.
#[async_trait]
pub(crate) trait LiveTranscriptExporter: Send + Sync {
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
}

impl LiveCaptureBinding {
    /// The runtime's only provider lookup: infallible, allocation-free and
    /// independent of configuration, so it adds no pre-ingress error path.
    pub(crate) fn resolve(provider: &dyn HookProvider) -> Self {
        Self::for_kind(provider.provider_name(), provider.agent_kind())
    }

    fn for_kind(hook_name: &'static str, kind: AgentKind) -> Self {
        Self {
            hook_name,
            kind,
            observed: agent_for(kind),
            live: live_capture_for(kind).unwrap_or(&NEUTRAL_LIVE_CAPTURE),
        }
    }

    /// The hook provider name: the session-id prefix and telemetry field.
    pub(crate) fn hook_name(self) -> &'static str {
        self.hook_name
    }

    /// The durable `agent_session.agent_kind` spelling of the typed identity.
    pub(crate) fn agent_kind_db(self) -> &'static str {
        self.kind.as_db_str()
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

    pub(crate) fn live_coverage_normalizer(self) -> Option<LiveCoverageNormalizer> {
        self.live.live_coverage_normalizer()
    }

    pub(crate) fn subagent_discovery(self) -> Option<&'static dyn LiveSubagentDiscovery> {
        self.live.subagent_discovery()
    }

    pub(crate) fn transcript_exporter(self) -> Option<&'static dyn LiveTranscriptExporter> {
        self.live.transcript_exporter()
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
        claude_session_file_candidate(context.verified_cwd, context.provider_session_id)
            .map_err(|_| LiveCandidateUnavailable)
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
}

fn claude_live_coverage(transcript: &RedactedBytes) -> Vec<NormalizedTurn> {
    normalize_claude_transcript(transcript.bytes())
}

/// Codex keeps its rollout normalizer for exactness; with no live candidate
/// its source is never authorized, so the gate is currently unreachable live.
pub(super) struct CodexLiveCapture;

impl LiveCaptureProvider for CodexLiveCapture {
    fn live_coverage_normalizer(&self) -> Option<LiveCoverageNormalizer> {
        Some(codex_live_coverage)
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
            // no exporter diagnostic is rendered.
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

            // effective_agent_capture_deadline: `provider_kind != "opencode"`.
            let legacy_budget = (db == "opencode").then(|| ExportLimits::default().deadline);
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

            // ingest_agent_traces_payload_with_scope: only
            // `agent_kind == "claude_code"` discovered child transcripts.
            assert_eq!(
                binding.subagent_discovery().is_some(),
                db == "claude_code",
                "{db}"
            );

            // write_committed_checkpoint: only `agent_kind == "opencode"`
            // entered the trusted export bridge, normalizing its bytes with
            // the OpenCode export normalizer.
            match binding.transcript_exporter() {
                Some(exporter) => {
                    assert_eq!(db, "opencode", "{db}: unexpected transcript exporter");
                    let fixture =
                        RedactedBytes::new_unchecked(OPENCODE_FIXTURE.as_bytes().to_vec());
                    let normalize = exporter.export_coverage_normalizer();
                    assert_eq!(normalize(&fixture).len(), 1, "{db}");
                    assert_eq!(
                        normalize(&fixture),
                        normalize_opencode_export(fixture.bytes()),
                        "{db}"
                    );
                }
                None => assert_ne!(db, "opencode", "{db}: missing transcript exporter"),
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
