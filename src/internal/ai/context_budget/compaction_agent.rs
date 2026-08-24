//! LLM-summary path for the embedded `compaction` agent.
//!
//! OC-Phase 4 P4.4 deliverable from `docs/development/commands/_general.md`. The
//! sister module [`super::handoff`] (P4.3) defines the
//! [`ContextHandoff`] value and the strict 8-section parser; this
//! module wires both ends together — feeding a session frame into a
//! [`CompletionModel`] under the
//! [`embedded compaction.md`](super::COMPACTION_AGENT_NAME) prompt and
//! returning a fully validated [`ContextHandoff`].
//!
//! Boundary rules from the doc:
//!
//! - The compaction agent **must not** call tools. The embedded
//!   profile sets `tools: []`; this runner additionally clears
//!   [`CompletionRequest::tools`] before dispatch so a misconfigured
//!   parent that hands us a populated request cannot leak its
//!   tool-list through.
//! - On any failure — empty caller-supplied transcript, provider
//!   error, empty response, an unexpected tool call (the agent
//!   must be tool-less), or a malformed summary template — we
//!   surface a typed error. We **never** silently fall back to the
//!   raw transcript — the caller has to decide whether to retry,
//!   escalate to the user, or drop the compaction.
//! - The model defaults to whatever the caller passes in. Inheriting
//!   from the parent agent vs. picking a dedicated compaction model
//!   is a OC-Phase 5 config concern; this module stays model-agnostic
//!   so unit tests can inject a deterministic fake.
//!
//! What this module owns:
//! - [`COMPACTION_AGENT_NAME`] — the literal `name:` value of the
//!   embedded profile, exported so the dispatcher can look the
//!   profile up by string without re-parsing the markdown.
//! - [`embedded_compaction_system_prompt`] — convenience accessor
//!   that pulls the body of the embedded profile out at compile time
//!   so callers building one-off compaction runs do not need to load
//!   the full profile router.
//! - [`run_compaction`] — the LLM-summary entry point.
//! - [`CompactionAgentError`] — the five typed failure modes.
//!
//! What this module is **not**:
//! - It does not decide *when* to compact. The
//!   [`super::ContextBudget::is_overflow`] (OC-Phase 4 P4.5) signals
//!   that.
//! - It does not perform the post-compaction message reorder
//!   (`filterCompacted`, OC-Phase 4 P4.5).
//! - It does not persist the resulting [`super::CompactionEvent`] —
//!   that lives in the dispatcher / session-store seam.

use chrono::Utc;
use uuid::Uuid;

use super::{
    frame::{ContextAttachmentRef, ContextFrameSegment},
    handoff::{ContextHandoff, ContextHandoffParseError, parse_handoff_template},
};
use crate::internal::ai::completion::{
    AssistantContent, CompletionError, CompletionModel, CompletionRequest, Message,
};

/// Stable name of the embedded compaction agent profile. Keep in sync
/// with the `name:` line of
/// `src/internal/ai/agent/profile/embedded/compaction.md`. The
/// dispatcher resolves the profile via this string so a rename in the
/// frontmatter is loud (the constant fails to match) instead of
/// silently shadowing the agent.
pub const COMPACTION_AGENT_NAME: &str = "compaction";

/// Raw markdown of the embedded compaction profile, baked at compile
/// time. Used by the test suite to verify the embedded prompt loads
/// cleanly through the profile parser; production code should prefer
/// [`embedded_compaction_system_prompt`] which extracts only the
/// system prompt body.
pub const EMBEDDED_COMPACTION_PROFILE: &str =
    include_str!("../agent/profile/embedded/compaction.md");

/// Extract the system-prompt body from
/// [`EMBEDDED_COMPACTION_PROFILE`] at runtime — the markdown still
/// has YAML frontmatter, so we strip the leading `---`-fenced block
/// and return the body that follows.
///
/// Fallback: if no closing fence is found (e.g. a pre-build copy
/// step stripped the frontmatter) the entire trimmed file is
/// returned. This keeps the production path resilient even when the
/// embedded asset was reshaped at build time; the
/// `embedded_compaction_profile_parses_with_canonical_metadata`
/// unit test catches frontmatter regressions in CI.
pub fn embedded_compaction_system_prompt() -> &'static str {
    static BODY: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    BODY.get_or_init(|| {
        let trimmed = EMBEDDED_COMPACTION_PROFILE.trim_start();
        if let Some(rest) = trimmed.strip_prefix("---")
            && let Some(end) = rest.find("---")
        {
            return rest[end + 3..].trim();
        }
        trimmed
    })
}

/// Failure modes from [`run_compaction`].
///
/// The dispatcher must surface every variant — none of them is
/// silently downgraded to "retry with raw transcript". The doc rule
/// is that a failed compaction is a hard signal, not an opportunity
/// to bypass the budget.
#[derive(Debug)]
pub enum CompactionAgentError {
    /// The caller passed an empty (or whitespace-only)
    /// `frame_contents`. Surfacing this BEFORE dispatch protects
    /// providers — notably Anthropic — that reject empty user text
    /// blocks at the API layer; failing locally with a precise
    /// error is more actionable than parsing a generic
    /// `Provider(_)` message later.
    EmptyInput,
    /// The provider returned an error (network, rate-limit, auth,
    /// schema mismatch on the wire). Forward verbatim so the
    /// caller's retry / classification policy (OC-Phase 4 retry
    /// policy in `tool_loop`) can decide.
    Provider(CompletionError),
    /// The provider responded but the response carried no content
    /// at all — empty content list, or only non-text parts the
    /// runtime cannot extract a summary from.
    EmptyResponse,
    /// The provider returned at least one tool-call part. The
    /// compaction agent is contractually tool-less (the embedded
    /// profile sets `tools: []` and [`run_compaction`] clears the
    /// outbound tool list), so a tool-call response is a hard
    /// contract violation rather than an "empty response" — surface
    /// it distinctly so the dispatcher can flag the offending model
    /// instead of treating the run as a generic failure.
    UnexpectedToolCall { tool_name: String },
    /// The provider responded with text but the text did not match
    /// the literal 8-section template. Wraps the parser error so
    /// the caller can render the missing / reordered sections to
    /// the operator.
    InvalidTemplate(ContextHandoffParseError),
}

impl std::fmt::Display for CompactionAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInput => {
                f.write_str("compaction agent input transcript is empty (blank or whitespace-only)")
            }
            Self::Provider(err) => write!(f, "compaction provider error: {err}"),
            Self::EmptyResponse => {
                f.write_str("compaction agent returned empty response (no text content)")
            }
            Self::UnexpectedToolCall { tool_name } => write!(
                f,
                "compaction agent attempted to call tool {tool_name:?}, but tools are forbidden for this agent",
            ),
            Self::InvalidTemplate(err) => {
                write!(f, "compaction agent produced an invalid summary: {err}")
            }
        }
    }
}

impl std::error::Error for CompactionAgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(err) => Some(err),
            Self::EmptyInput | Self::EmptyResponse | Self::UnexpectedToolCall { .. } => None,
            Self::InvalidTemplate(err) => Some(err),
        }
    }
}

/// Run the embedded compaction agent against a rendered transcript
/// and return a validated [`ContextHandoff`].
///
/// # Arguments
/// * `model` — any [`CompletionModel`]; in production this is the
///   parent agent's binding by default, in tests this is a
///   deterministic fake.
/// * `system_prompt` — body of the compaction profile. Callers will
///   typically pass [`embedded_compaction_system_prompt`] but a
///   profile loaded from disk works the same way (S5 will allow
///   per-project overrides). Sent as
///   [`CompletionRequest::preamble`] (not as a `Message::System`)
///   so every provider — including Gemini, which only hoists
///   `preamble` into `system_instruction` — receives the prompt in
///   the system role.
/// * `frame_contents` — the rendered transcript the agent should
///   summarise. Composition (segment ordering, bullet rendering) is
///   the dispatcher's responsibility, not this module's.
/// * `source_frame_id` — id of the [`super::ContextFrameEvent`] that
///   produced `frame_contents`. **Caller invariant**: this must be
///   the same frame, not a downstream frame the dispatcher has
///   already moved on to. Stored verbatim on the resulting handoff
///   so replay can match the transcript without traversing JSONL.
///   The runtime cannot enforce this from the strings alone.
/// * `attachment_refs` — file / blob references the dispatcher
///   already extracted from the frame. Forwarded as-is so the
///   summary citations stay re-readable.
/// * `recent_tail` — segments the dispatcher decided belong to the
///   "retained tail" (cf. P4.5 `filterCompacted` ordering rule).
///   Carried through opaquely.
/// * `remaining_budget_tokens` — the budget calculator's current
///   estimate. The receiving runtime uses this to size the next
///   request without re-running the calculation.
///
/// # Errors
/// See [`CompactionAgentError`]. A tool-call response is rejected
/// with [`CompactionAgentError::UnexpectedToolCall`] BEFORE the
/// summary parser runs — the doc rule is no-tools, so a tool-call
/// is a contract violation that takes precedence over template
/// validation.
pub async fn run_compaction<M>(
    model: &M,
    system_prompt: &str,
    frame_contents: &str,
    source_frame_id: Uuid,
    attachment_refs: Vec<ContextAttachmentRef>,
    recent_tail: Vec<ContextFrameSegment>,
    remaining_budget_tokens: u64,
) -> Result<ContextHandoff, CompactionAgentError>
where
    M: CompletionModel,
{
    if frame_contents.trim().is_empty() {
        return Err(CompactionAgentError::EmptyInput);
    }

    let request = build_request(system_prompt, frame_contents);
    let response = model
        .completion(request)
        .await
        .map_err(CompactionAgentError::Provider)?;

    if let Some(tool_call) = first_tool_call(&response.content) {
        return Err(CompactionAgentError::UnexpectedToolCall {
            tool_name: tool_call.name.clone(),
        });
    }

    let summary =
        extract_text_content(&response.content).ok_or(CompactionAgentError::EmptyResponse)?;
    parse_handoff_template(&summary).map_err(CompactionAgentError::InvalidTemplate)?;

    Ok(ContextHandoff {
        summary,
        recent_tail,
        attachment_refs,
        source_frame_id,
        remaining_budget_tokens,
        created_at: Utc::now(),
    })
}

/// Build the conversation the compaction agent runs against. The
/// system prompt rides in [`CompletionRequest::preamble`] — every
/// provider hoists `preamble` into its native system slot, including
/// Gemini whose `Message::System` mapping (see
/// [`crate::internal::ai::providers::gemini::completion`]) silently
/// downgrades inline system messages to user-role content. Tools are
/// explicitly cleared so a contract violation by the parent (passing
/// in tools on the outer request) cannot accidentally promote this
/// agent into a tool-using surface.
fn build_request(system_prompt: &str, frame_contents: &str) -> CompletionRequest {
    CompletionRequest {
        preamble: Some(system_prompt.to_string()),
        chat_history: vec![Message::user(frame_contents)],
        tools: Vec::new(),
        ..CompletionRequest::default()
    }
}

/// Find the first tool-call part in the assistant content list. The
/// caller uses this for the no-tools contract check; we report the
/// **first** tool call's name (rather than aggregating) because
/// most providers can only emit one such part per response and the
/// first one is the most actionable signal for the user.
fn first_tool_call(
    content: &[AssistantContent],
) -> Option<&crate::internal::ai::completion::ToolCall> {
    content.iter().find_map(|part| match part {
        AssistantContent::ToolCall(tc) => Some(tc),
        AssistantContent::Text(_) => None,
    })
}

/// Build a [`super::CompactionEvent`] from a successful
/// [`run_compaction`] outcome. Centralises the frame → event
/// conversion the dispatcher (OC-Phase 3) needs to apply on the
/// `Ok` path of the compaction agent. Calling this helper from the
/// `Err` path would be a logic error — the caller's responsibility
/// is to gate on the `Result`, not to construct an event for a
/// failed run. Keeping the conversion here means tests and the
/// production dispatcher exercise the same code, and the doc rule
/// "失败时不写入 CompactionEvent" stays a property of the helper's
/// signature (it requires a successful `ContextHandoff` as input).
pub fn compaction_event_for_handoff(
    frame: &super::ContextFrameEvent,
    handoff: &ContextHandoff,
    reason: super::CompactionReason,
    tail_start_id: Option<&str>,
) -> super::CompactionEvent {
    let event = super::CompactionEvent::from_frame(frame, reason, handoff.summary.clone());
    match tail_start_id {
        Some(id) => event.with_tail_start_id(id),
        None => event,
    }
}

/// Concatenate every text part in the assistant content list into a
/// single owned `String`. Returns `None` when the list is empty or
/// when every part is a tool call (the [`first_tool_call`] gate
/// above already runs first, so a `None` return here strictly means
/// "no extractable content at all").
fn extract_text_content(content: &[AssistantContent]) -> Option<String> {
    let mut buffer = String::new();
    for part in content {
        if let AssistantContent::Text(text) = part {
            buffer.push_str(&text.text);
        }
    }
    if buffer.is_empty() {
        None
    } else {
        Some(buffer)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::internal::ai::{
        agent::profile::parse_agent_profile,
        completion::{CompletionResponse, CompletionUsage, CompletionUsageSummary, Text},
    };

    /// A minimal `CompletionModel` impl that records the inbound
    /// request and returns a canned reply. Pure synchronous —
    /// returning `Ready` from the future avoids spinning up a
    /// runtime in unit tests.
    ///
    /// `error_message` is stored as a `String` rather than a
    /// [`CompletionError`] because the latter does not implement
    /// `Clone` and we want the fixture to return the same error on
    /// every dispatch without per-clone gymnastics.
    #[derive(Clone)]
    struct CannedModel {
        reply: Vec<AssistantContent>,
        captured: std::sync::Arc<Mutex<Option<CompletionRequest>>>,
        error_message: Option<String>,
    }

    impl CannedModel {
        fn text(reply: impl Into<String>) -> Self {
            Self {
                reply: vec![AssistantContent::Text(Text { text: reply.into() })],
                captured: std::sync::Arc::new(Mutex::new(None)),
                error_message: None,
            }
        }

        fn empty() -> Self {
            Self {
                reply: Vec::new(),
                captured: std::sync::Arc::new(Mutex::new(None)),
                error_message: None,
            }
        }

        fn with_provider_error(message: impl Into<String>) -> Self {
            Self {
                reply: Vec::new(),
                captured: std::sync::Arc::new(Mutex::new(None)),
                error_message: Some(message.into()),
            }
        }

        fn captured_request(&self) -> Option<CompletionRequest> {
            self.captured.lock().unwrap().clone()
        }
    }

    #[derive(Debug)]
    struct CannedRaw;

    impl CompletionUsage for CannedRaw {
        fn usage_summary(&self) -> Option<CompletionUsageSummary> {
            None
        }
    }

    impl CompletionModel for CannedModel {
        type Response = CannedRaw;

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            *self.captured.lock().unwrap() = Some(request);
            if let Some(message) = &self.error_message {
                return Err(CompletionError::ProviderError(message.clone()));
            }
            Ok(CompletionResponse {
                content: self.reply.clone(),
                reasoning_content: None,
                raw_response: CannedRaw,
            })
        }
    }

    /// Canonical 8-section template the canned model returns when
    /// the test exercises the happy path.
    const VALID_SUMMARY: &str = "\
## Goal
- Add unit test for utils::path::join

## Constraints & Preferences
- Stick to the existing snapshot harness

## Progress
### Done
- Located the helper

### In Progress
- Drafting the failure-mode case

### Blocked
- (none)

## Key Decisions
- Use proptest for random separators

## Next Steps
- Wire the new test module into mod.rs

## Critical Context
- Existing test runner does not propagate panics

## Relevant Files
- src/utils/path.rs: target of the new test
";

    /// Scenario: the embedded compaction.md frontmatter is parseable
    /// by the production profile parser, declares the canonical
    /// `name: compaction`, requests an empty tool list, and uses
    /// the subagent mode so it never appears as a primary-eligible
    /// option.
    #[test]
    fn embedded_compaction_profile_parses_with_canonical_metadata() {
        let profile = parse_agent_profile(EMBEDDED_COMPACTION_PROFILE)
            .expect("embedded compaction profile must parse");
        assert_eq!(profile.name, COMPACTION_AGENT_NAME);
        assert!(
            profile.tools.is_empty(),
            "compaction agent must declare no tools, got {:?}",
            profile.tools
        );
        assert_eq!(
            profile.mode,
            crate::internal::ai::agent::profile::AgentMode::Subagent,
            "compaction agent must be subagent-only so it cannot be selected as primary"
        );
    }

    /// Scenario: the literal SUMMARY_TEMPLATE in the embedded prompt
    /// must be byte-for-byte what `parse_handoff_template` expects.
    /// If a copy-edit drifts the heading text (`## Goal` → `## Goals`)
    /// the agent would emit a summary the runtime cannot parse, so
    /// the embedded prompt itself has to carry every required
    /// heading.
    #[test]
    fn embedded_compaction_prompt_contains_every_required_heading() {
        let prompt = embedded_compaction_system_prompt();
        for heading in [
            "## Goal",
            "## Constraints & Preferences",
            "## Progress",
            "### Done",
            "### In Progress",
            "### Blocked",
            "## Key Decisions",
            "## Next Steps",
            "## Critical Context",
            "## Relevant Files",
        ] {
            assert!(
                prompt.contains(heading),
                "embedded compaction prompt missing required heading: {heading}"
            );
        }
    }

    /// Scenario: the runner routes the system prompt through
    /// `preamble`, sends a user-only chat history, clears tools to
    /// enforce the no-tools contract, and returns a fully populated
    /// [`ContextHandoff`] when the model responds with a valid
    /// template.
    #[tokio::test]
    async fn run_compaction_returns_handoff_on_valid_summary() {
        let model = CannedModel::text(VALID_SUMMARY);
        let frame_id = Uuid::new_v4();
        let handoff = run_compaction(
            &model,
            "system: be terse",
            "user: summarise this transcript",
            frame_id,
            Vec::new(),
            Vec::new(),
            12_345,
        )
        .await
        .expect("valid template must parse");

        assert_eq!(handoff.summary, VALID_SUMMARY);
        assert_eq!(handoff.source_frame_id, frame_id);
        assert_eq!(handoff.remaining_budget_tokens, 12_345);

        let captured = model.captured_request().expect("model was invoked");
        assert!(
            captured.tools.is_empty(),
            "compaction agent must clear tools list, got {:?}",
            captured.tools
        );
        assert_eq!(
            captured.preamble.as_deref(),
            Some("system: be terse"),
            "compaction agent must route system prompt through preamble for cross-provider parity, got {:?}",
            captured.preamble,
        );
        assert_eq!(
            captured.chat_history.len(),
            1,
            "expected user-only chat history (system rides in preamble), got {} messages",
            captured.chat_history.len()
        );
        assert!(matches!(captured.chat_history[0], Message::User { .. }));
    }

    /// Scenario: a model that returns a tool-call instead of text
    /// is rejected with the distinct `UnexpectedToolCall` variant
    /// (not the generic `EmptyResponse`). The compaction agent is
    /// contractually tool-less; surfacing the tool name lets the
    /// dispatcher flag the offending model in operator-facing
    /// telemetry.
    #[tokio::test]
    async fn run_compaction_rejects_tool_call_response() {
        use crate::internal::ai::completion::{Function, ToolCall};
        let model = CannedModel {
            reply: vec![AssistantContent::ToolCall(ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                function: Function {
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "/tmp/foo"}),
                },
            })],
            captured: std::sync::Arc::new(Mutex::new(None)),
            error_message: None,
        };
        let err = run_compaction(
            &model,
            "system",
            "user",
            Uuid::new_v4(),
            Vec::new(),
            Vec::new(),
            0,
        )
        .await
        .unwrap_err();
        match err {
            CompactionAgentError::UnexpectedToolCall { tool_name } => {
                assert_eq!(tool_name, "read_file");
            }
            other => panic!("expected UnexpectedToolCall, got {other:?}"),
        }
    }

    /// Scenario: a populated [`ContextHandoff`] — including
    /// non-empty `recent_tail` and `attachment_refs` — round-trips
    /// through JSON without losing fields. The fixture deliberately
    /// stresses two `ContextFrameSegment` shapes the production
    /// runtime emits:
    ///
    /// 1. content-bearing in-line segment (`content: Some`,
    ///    `summary: None`, `attachment: Some`, `non_compressible:
    ///    false`, source detail absent),
    /// 2. attached-content summary segment (`content: None`,
    ///    `summary: Some`, `attachment: Some`, `non_compressible:
    ///    true`, source detail present).
    ///
    /// Covering both keeps a future field-add or `Option`-shape
    /// regression visible in this test rather than only firing in
    /// production.
    #[test]
    fn context_handoff_json_round_trip() {
        use crate::internal::ai::context_budget::{
            ContextFrameSource, ContextSegmentKind, ContextTrustLevel,
        };

        let attachment = ContextAttachmentRef {
            sha256: "abc123".to_string(),
            bytes: 4_096,
            line_count: 128,
            relative_path: "src/lib.rs".to_string(),
            read_hint: "head".to_string(),
        };
        let inline_segment = ContextFrameSegment {
            id: "seg-1".to_string(),
            segment: ContextSegmentKind::RecentMessages,
            source: ContextFrameSource::runtime("transcript"),
            trust: ContextTrustLevel::Trusted,
            token_estimate: 512,
            content: Some("recent user turn".to_string()),
            summary: None,
            attachment: Some(attachment.clone()),
            non_compressible: false,
        };
        let attached_segment = ContextFrameSegment {
            id: "seg-2".to_string(),
            segment: ContextSegmentKind::ToolResults,
            source: ContextFrameSource::tool("read_file", "src/lib.rs"),
            trust: ContextTrustLevel::Untrusted,
            token_estimate: 8_192,
            content: None,
            summary: Some("read_file: 128 lines".to_string()),
            attachment: Some(attachment.clone()),
            non_compressible: true,
        };
        let original = ContextHandoff {
            summary: VALID_SUMMARY.to_string(),
            recent_tail: vec![inline_segment, attached_segment],
            attachment_refs: vec![attachment],
            source_frame_id: Uuid::new_v4(),
            remaining_budget_tokens: 4_096,
            created_at: Utc::now(),
        };
        let serialised = serde_json::to_string(&original).expect("serialize");
        let restored: ContextHandoff = serde_json::from_str(&serialised).expect("deserialize");
        assert_eq!(restored, original);
    }

    /// Scenario: a hand-rolled JSON document using the **stable**
    /// snake_case wire keys deserializes into the expected struct.
    /// This is the wire-schema lock that catches a future
    /// `#[serde(rename)]` or field rename which would break JSONL
    /// persisted by older versions even though
    /// [`context_handoff_json_round_trip`] would still pass
    /// (because round-trip uses the same code on both sides).
    ///
    /// To lock **nested** wire names too — `ContextFrameSegment`
    /// fields, `ContextFrameSource` fields, and the snake_case enum
    /// values for `ContextSegmentKind` / `ContextTrustLevel` /
    /// `ContextFrameSourceKind` — the fixture writes one literal
    /// segment into `recent_tail` and asserts each nested field
    /// round-tripped to its expected value.
    #[test]
    fn context_handoff_deserializes_from_stable_wire_format() {
        let frame_id = Uuid::nil();
        let value = serde_json::json!({
            "summary": "## Goal\n- demo",
            "recent_tail": [{
                "id": "seg-stable",
                "segment": "recent_messages",
                "source": {
                    "kind": "tool",
                    "label": "read_file",
                    "detail": "src/lib.rs"
                },
                "trust": "untrusted",
                "token_estimate": 256,
                "summary": "read_file: 32 lines",
                "non_compressible": true
            }],
            "attachment_refs": [{
                "sha256": "deadbeef",
                "bytes": 1024,
                "line_count": 16,
                "relative_path": "README.md",
                "read_hint": "tail"
            }],
            "source_frame_id": frame_id,
            "remaining_budget_tokens": 8192,
            "created_at": "2026-05-07T00:00:00Z",
        });
        let parsed: ContextHandoff =
            serde_json::from_value(value).expect("stable wire format must deserialize");

        // Top-level field round-trip
        assert_eq!(parsed.summary, "## Goal\n- demo");
        assert_eq!(parsed.source_frame_id, frame_id);
        assert_eq!(parsed.remaining_budget_tokens, 8192);

        // ContextAttachmentRef wire keys
        assert_eq!(parsed.attachment_refs.len(), 1);
        let att = &parsed.attachment_refs[0];
        assert_eq!(att.sha256, "deadbeef");
        assert_eq!(att.bytes, 1024);
        assert_eq!(att.line_count, 16);
        assert_eq!(att.relative_path, "README.md");
        assert_eq!(att.read_hint, "tail");

        // ContextFrameSegment wire keys + nested ContextFrameSource
        assert_eq!(parsed.recent_tail.len(), 1);
        let seg = &parsed.recent_tail[0];
        assert_eq!(seg.id, "seg-stable");
        assert_eq!(
            seg.segment,
            crate::internal::ai::context_budget::ContextSegmentKind::RecentMessages
        );
        assert_eq!(
            seg.source.kind,
            crate::internal::ai::context_budget::frame::ContextFrameSourceKind::Tool
        );
        assert_eq!(seg.source.label, "read_file");
        assert_eq!(seg.source.detail.as_deref(), Some("src/lib.rs"));
        assert_eq!(
            seg.trust,
            crate::internal::ai::context_budget::ContextTrustLevel::Untrusted
        );
        assert_eq!(seg.token_estimate, 256);
        assert!(seg.content.is_none());
        assert_eq!(seg.summary.as_deref(), Some("read_file: 32 lines"));
        assert!(seg.attachment.is_none());
        assert!(seg.non_compressible);
    }

    /// Scenario: an empty (or whitespace-only) `frame_contents`
    /// fails fast with `EmptyInput` BEFORE the model is invoked.
    /// This protects providers that reject empty user-text blocks
    /// at the API layer (Anthropic in particular) and surfaces a
    /// precise error rather than a generic `Provider(_)` upstream.
    #[tokio::test]
    async fn run_compaction_rejects_empty_input() {
        let model = CannedModel::text(VALID_SUMMARY);
        for blank in ["", "   ", "\n\n", "\t \n"] {
            let err = run_compaction(
                &model,
                "system",
                blank,
                Uuid::new_v4(),
                Vec::new(),
                Vec::new(),
                0,
            )
            .await
            .unwrap_err();
            assert!(
                matches!(err, CompactionAgentError::EmptyInput),
                "blank frame_contents {blank:?} must surface as EmptyInput, got {err:?}"
            );
        }
        assert!(
            model.captured_request().is_none(),
            "model must not be invoked when frame_contents is blank"
        );
    }

    /// Scenario: a model that returns a fully empty content list
    /// (no parts at all) surfaces as `EmptyResponse`. Tool-call
    /// responses take a different path (`UnexpectedToolCall`,
    /// covered by [`run_compaction_rejects_tool_call_response`]);
    /// this test guards the truly-no-content branch. The runtime
    /// explicitly does NOT fall back to the raw transcript in this
    /// case — the doc rule is that a failed compaction is a hard
    /// signal.
    #[tokio::test]
    async fn run_compaction_rejects_empty_response() {
        let model = CannedModel::empty();
        let err = run_compaction(
            &model,
            "system",
            "user",
            Uuid::new_v4(),
            Vec::new(),
            Vec::new(),
            0,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CompactionAgentError::EmptyResponse));
    }

    /// Scenario: a model response that misses a section bubbles up
    /// as `InvalidTemplate(SchemaMismatch)` so the dispatcher can
    /// render which section is missing to the operator.
    #[tokio::test]
    async fn run_compaction_rejects_summary_with_missing_section() {
        let truncated = VALID_SUMMARY.replace(
            "## Critical Context\n- Existing test runner does not propagate panics\n\n",
            "",
        );
        let model = CannedModel::text(&truncated);
        let err = run_compaction(
            &model,
            "system",
            "user",
            Uuid::new_v4(),
            Vec::new(),
            Vec::new(),
            0,
        )
        .await
        .unwrap_err();
        match err {
            CompactionAgentError::InvalidTemplate(ContextHandoffParseError::SchemaMismatch {
                missing_sections,
            }) => {
                assert_eq!(missing_sections, vec!["## Critical Context".to_string()]);
            }
            other => panic!("expected InvalidTemplate(SchemaMismatch), got {other:?}"),
        }
    }

    /// Scenario: a provider-level error (network, auth, etc.)
    /// surfaces as `Provider(_)` verbatim so the OC-Phase 4 retry
    /// policy in `tool_loop` can classify and act on it.
    #[tokio::test]
    async fn run_compaction_forwards_provider_error() {
        let model = CannedModel::with_provider_error("rate limited");
        let err = run_compaction(
            &model,
            "system",
            "user",
            Uuid::new_v4(),
            Vec::new(),
            Vec::new(),
            0,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CompactionAgentError::Provider(_)));
    }

    /// Scenario: `Display` impls render every variant with enough
    /// context to be actionable in a Code UI log line.
    #[test]
    fn compaction_agent_error_display_renders_each_variant() {
        let empty_input = CompactionAgentError::EmptyInput;
        let formatted_input = format!("{empty_input}");
        assert!(formatted_input.contains("empty"));
        assert!(formatted_input.contains("transcript"));
        // Don't leak the internal parameter name into the
        // user-facing Code UI string.
        assert!(
            !formatted_input.contains("frame_contents"),
            "EmptyInput display must not expose internal parameter name, got {formatted_input:?}"
        );

        let provider =
            CompactionAgentError::Provider(CompletionError::ProviderError("boom".to_string()));
        assert!(format!("{provider}").contains("provider"));

        let empty = CompactionAgentError::EmptyResponse;
        assert!(format!("{empty}").contains("empty"));

        let tool_call = CompactionAgentError::UnexpectedToolCall {
            tool_name: "read_file".to_string(),
        };
        let formatted_tool = format!("{tool_call}");
        assert!(formatted_tool.contains("read_file"));
        assert!(formatted_tool.contains("forbidden"));

        let bad = CompactionAgentError::InvalidTemplate(ContextHandoffParseError::SchemaMismatch {
            missing_sections: vec!["## Goal".to_string()],
        });
        let formatted = format!("{bad}");
        assert!(formatted.contains("invalid summary"));
        assert!(formatted.contains("## Goal"));
    }

    #[test]
    fn compaction_agent_error_display_pins_each_variant() {
        assert_eq!(
            CompactionAgentError::EmptyInput.to_string(),
            "compaction agent input transcript is empty (blank or whitespace-only)",
        );
        assert_eq!(
            CompactionAgentError::EmptyResponse.to_string(),
            "compaction agent returned empty response (no text content)",
        );
        assert_eq!(
            CompactionAgentError::UnexpectedToolCall {
                tool_name: "apply_patch".to_string(),
            }
            .to_string(),
            "compaction agent attempted to call tool \"apply_patch\", \
             but tools are forbidden for this agent",
        );
        assert_eq!(
            CompactionAgentError::InvalidTemplate(ContextHandoffParseError::DuplicateHeading {
                heading: "## Goal".to_string(),
            })
            .to_string(),
            "compaction agent produced an invalid summary: \
             context handoff summary contains duplicate heading: ## Goal",
        );
    }
}
