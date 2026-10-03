//! Canonical lifecycle event types and shared hook-ingestion helpers.
//!
//! Provider-specific hook adapters (Claude, Gemini, etc.) parse their own JSON wire
//! formats and lower them into the agent-agnostic [`LifecycleEvent`] structure
//! defined here. Downstream code (session storage, projection, audit logs) consumes
//! only this normalised form, so adding a new provider does not require changes to
//! the rest of the agent pipeline.
//!
//! Helpers in this module also enforce envelope validation (preventing path
//! traversal in `session_id`, capping `transcript_path` length) and produce stable
//! dedup keys so duplicate hook deliveries can be filtered out at ingestion time.

use std::{collections::BTreeMap, fmt};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::internal::ai::{event::Event, session::SessionState};

/// Agent-agnostic lifecycle event kinds.
///
/// These map roughly onto the union of every provider's hook taxonomy. Variants are
/// intentionally finer-grained than the public `HookEvent` enum so that internal
/// projection logic can distinguish, for example, `TurnStart` (a new user prompt
/// arrived) from `Compaction` (the model summarised history to fit its context
/// window).
/// AG-19: `#[non_exhaustive]` so downstream matches must carry a fallback
/// arm — adding a lifecycle kind is an additive, non-breaking change.
/// New variants must be **appended at the end**: `event_id()` folds
/// `kind as u8` into its UUIDv5 name, so reordering renumbers every
/// previously persisted event id.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleEventKind {
    SessionStart,
    TurnStart,
    ToolUse,
    ModelUpdate,
    Compaction,
    CompactionCompleted,
    PermissionRequest,
    SourceEnabled,
    SourceDisabled,
    TurnEnd,
    SessionEnd,
    /// A sub-agent (nested agent run inside the same provider session)
    /// started (AG-19; synthesized from transcript analysis or provider
    /// envelopes that carry a subagent id — not a first-class hook
    /// command for any current provider).
    SubagentStart,
    /// A sub-agent finished (AG-19; see [`Self::SubagentStart`]).
    SubagentEnd,
}

impl fmt::Display for LifecycleEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            LifecycleEventKind::SessionStart => "session_start",
            LifecycleEventKind::TurnStart => "turn_start",
            LifecycleEventKind::ToolUse => "tool_use",
            LifecycleEventKind::ModelUpdate => "model_update",
            LifecycleEventKind::Compaction => "compaction",
            LifecycleEventKind::CompactionCompleted => "compaction_completed",
            LifecycleEventKind::PermissionRequest => "permission_request",
            LifecycleEventKind::SourceEnabled => "source_enabled",
            LifecycleEventKind::SourceDisabled => "source_disabled",
            LifecycleEventKind::TurnEnd => "turn_end",
            LifecycleEventKind::SessionEnd => "session_end",
            LifecycleEventKind::SubagentStart => "subagent_start",
            LifecycleEventKind::SubagentEnd => "subagent_end",
        };
        write!(f, "{value}")
    }
}

/// A normalized lifecycle event produced by a provider hook adapter.
///
/// All optional fields are populated only when the source envelope supplies them.
/// `timestamp` is recorded at parse time, not from the envelope, to keep ordering
/// authoritative even when providers omit it.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleEvent {
    pub kind: LifecycleEventKind,
    pub session_id: String,
    pub session_ref: Option<String>,
    pub prompt: Option<String>,
    pub model: Option<Value>,
    pub source: Option<Value>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_response: Option<Value>,
    pub assistant_message: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// Fixed v4 UUID acting as the namespace for `Uuid::new_v5` derivation of
/// `LifecycleEvent::event_id`.
///
/// **Stability contract**: this constant **must not change**. Any change is
/// equivalent to renumbering every previously-emitted lifecycle event id and
/// would break audit-log dedupe / correlation. If a future migration ever
/// needs a different namespace, ship it as `LIFECYCLE_EVENT_NAMESPACE_V2` and
/// version the `event_id` derivation explicitly.
const LIFECYCLE_EVENT_NAMESPACE: Uuid = Uuid::from_bytes([
    0x4d, 0xe6, 0x3a, 0x6b, // libra
    0x8a, 0x12, // lifecycle
    0x4f, 0x77, // namespace v1
    0x9c, 0x4b, // (random — frozen)
    0x05, 0x02, 0x06, 0x01, 0x10, 0x05, // 2026-05-02 round 2
]);

impl Event for LifecycleEvent {
    fn event_kind(&self) -> &'static str {
        // Stable wire kinds — keep this match in sync with
        // `LifecycleEventKind::Display` so audit / projection consumers see
        // a single canonical form.
        match self.kind {
            LifecycleEventKind::SessionStart => "session_start",
            LifecycleEventKind::TurnStart => "turn_start",
            LifecycleEventKind::ToolUse => "tool_use",
            LifecycleEventKind::ModelUpdate => "model_update",
            LifecycleEventKind::Compaction => "compaction",
            LifecycleEventKind::CompactionCompleted => "compaction_completed",
            LifecycleEventKind::PermissionRequest => "permission_request",
            LifecycleEventKind::SourceEnabled => "source_enabled",
            LifecycleEventKind::SourceDisabled => "source_disabled",
            LifecycleEventKind::TurnEnd => "turn_end",
            LifecycleEventKind::SessionEnd => "session_end",
            LifecycleEventKind::SubagentStart => "subagent_start",
            LifecycleEventKind::SubagentEnd => "subagent_end",
        }
    }

    fn event_id(&self) -> Uuid {
        // Lifecycle events do not have a provider-assigned UUID, but generic
        // dedupe / indexing code expects `event_id()` to be stable per
        // occurrence. Derive a deterministic UUID v5 from the event's
        // natural identity tuple `(session_id, timestamp_nanos, kind)` so
        // two structurally identical events hash to the same id and two
        // distinct events do not collide. (CEX-00.5 Codex review P2 fix,
        // round 2: switched from `DefaultHasher` to `Uuid::new_v5` because
        // `DefaultHasher` is documented as not stable across Rust releases
        // and `event_id` may end up persisted in audit logs.)
        //
        // `LIFECYCLE_EVENT_NAMESPACE` is a fixed v4 UUID generated for the
        // libra runtime; it acts as the SHA-1 namespace for v5 derivation
        // and **must not change** without a coordinated audit-log migration.
        let mut name = Vec::with_capacity(self.session_id.len() + 32);
        name.extend_from_slice(self.session_id.as_bytes());
        name.push(0u8);
        name.extend_from_slice(
            &self
                .timestamp
                .timestamp_nanos_opt()
                .unwrap_or(0)
                .to_be_bytes(),
        );
        name.push(0u8);
        name.push(self.kind as u8);
        Uuid::new_v5(&LIFECYCLE_EVENT_NAMESPACE, &name)
    }

    fn event_summary(&self) -> String {
        let mut parts: Vec<String> = vec![
            format!("kind={}", self.kind),
            format!("session={}", self.session_id),
        ];
        if let Some(tool) = self.tool_name.as_deref() {
            parts.push(format!("tool={tool}"));
        }
        if let Some(model) = self.model.as_ref().and_then(|v| v.as_str()) {
            parts.push(format!("model={model}"));
        }
        parts.join(" ")
    }
}

/// Common hook payload envelope shared by provider-specific parsers.
///
/// The `#[serde(flatten)]` `extra` map captures any provider-specific keys that the
/// canonical envelope does not name explicitly, allowing downstream parsers to look
/// up `tool_name`, `prompt`, `model`, etc. without forcing every provider to share
/// an identical wire schema.
#[derive(Debug, Deserialize, Clone)]
pub struct SessionHookEnvelope {
    pub hook_event_name: String,
    pub session_id: String,
    pub cwd: String,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Core envelope validation shared by all providers.
///
/// Functional scope:
/// - Verifies the four required fields (`hook_event_name`, `session_id`, `cwd`,
///   and optionally a non-empty `transcript_path`) are populated.
/// - Restricts `session_id` to a safe ASCII alphabet so it can be used as part of
///   on-disk paths without sanitisation.
/// - Caps `transcript_path` length and rejects embedded NUL bytes to defend
///   downstream FS code from unsafe paths.
///
/// Boundary conditions:
/// - Returns `Err` (via `bail!`) on the first failed check; callers see one
///   actionable message rather than a list.
/// - Whitespace-only fields are treated as empty.
pub fn validate_session_hook_envelope(
    envelope: &SessionHookEnvelope,
    max_transcript_path_bytes: usize,
) -> Result<()> {
    if envelope.hook_event_name.trim().is_empty() {
        bail!("missing required field: hook_event_name");
    }
    if envelope.session_id.trim().is_empty() {
        bail!("missing required field: session_id");
    }
    validate_session_id(&envelope.session_id)?;
    if envelope.cwd.trim().is_empty() {
        bail!("missing required field: cwd");
    }
    if let Some(transcript_path) = envelope.transcript_path.as_deref() {
        validate_transcript_path(transcript_path, max_transcript_path_bytes)?;
    }
    Ok(())
}

/// Apply a normalized lifecycle event to the in-memory session state.
///
/// Functional scope:
/// - Mutates `session` in place to reflect the event:
///   - `SessionStart` / `ModelUpdate` write `model` and `source` metadata.
///   - `TurnStart` appends a user message when a prompt is present.
///   - `ToolUse` appends to a bounded `tool_events` array.
///   - `Compaction` increments a counter so the UI can flag context compaction.
///   - `TurnEnd` records the final assistant message.
///   - `SessionEnd` is a no-op marker (state is flushed by the caller).
///
/// Boundary conditions:
/// - The `tool_events` array defensively overwrites a malformed metadata slot
///   rather than panicking.
/// - `tool_events` is capped at `max_tool_events`; oldest entries are dropped to
///   keep memory and persistence size bounded.
pub fn apply_lifecycle_event(
    session: &mut SessionState,
    event: &LifecycleEvent,
    max_tool_events: usize,
) {
    match event.kind {
        LifecycleEventKind::SessionStart => {
            if let Some(model) = &event.model {
                session
                    .metadata
                    .insert("model".to_string(), normalize_json_value(model.clone()));
            }
            if let Some(source) = &event.source {
                session
                    .metadata
                    .insert("source".to_string(), normalize_json_value(source.clone()));
            }
        }
        LifecycleEventKind::TurnStart => {
            if let Some(prompt) = &event.prompt {
                session.add_user_message(prompt);
            }
        }
        LifecycleEventKind::ToolUse => {
            let tool_event = json!({
                "name": event.tool_name,
                "input": event.tool_input,
                "response": event.tool_response,
                "timestamp": event.timestamp.to_rfc3339(),
            });

            let entry = session
                .metadata
                .entry("tool_events".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let Value::Array(items) = entry else {
                session
                    .metadata
                    .insert("tool_events".to_string(), Value::Array(vec![tool_event]));
                return;
            };
            items.push(tool_event);
            if items.len() > max_tool_events {
                let drop_n = items.len() - max_tool_events;
                items.drain(0..drop_n);
            }
        }
        LifecycleEventKind::ModelUpdate => {
            if let Some(model) = &event.model {
                session
                    .metadata
                    .insert("model".to_string(), normalize_json_value(model.clone()));
            }
        }
        LifecycleEventKind::Compaction => {
            let current = session
                .metadata
                .get("compaction_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            session
                .metadata
                .insert("compaction_count".to_string(), json!(current + 1));
        }
        LifecycleEventKind::CompactionCompleted => {
            session.metadata.insert(
                "last_compaction_completed_at".to_string(),
                json!(event.timestamp),
            );
        }
        LifecycleEventKind::PermissionRequest
        | LifecycleEventKind::SourceEnabled
        | LifecycleEventKind::SourceDisabled => {
            let entry = json!({
                "kind": event.kind.to_string(),
                "source": event.source,
                "tool": event.tool_name,
                "timestamp": event.timestamp.to_rfc3339(),
            });
            let slot = session
                .metadata
                .entry("automation_events".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let Value::Array(items) = slot else {
                session
                    .metadata
                    .insert("automation_events".to_string(), Value::Array(vec![entry]));
                return;
            };
            items.push(entry);
        }
        LifecycleEventKind::TurnEnd => {
            if let Some(message) = &event.assistant_message {
                session.add_assistant_message(message);
                session
                    .metadata
                    .insert("last_assistant_message".to_string(), json!(message));
            }
        }
        LifecycleEventKind::SessionEnd => {}
        // AG-19: sub-agent boundaries are recorded as bounded metadata
        // entries (mirroring automation_events) so a parent session's
        // projection can see nested runs without a schema change.
        LifecycleEventKind::SubagentStart | LifecycleEventKind::SubagentEnd => {
            let entry = json!({
                "kind": event.kind.to_string(),
                "source": event.source,
                "tool": event.tool_name,
                "timestamp": event.timestamp.to_rfc3339(),
            });
            let slot = session
                .metadata
                .entry("subagent_events".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let Value::Array(items) = slot else {
                session
                    .metadata
                    .insert("subagent_events".to_string(), Value::Array(vec![entry]));
                return;
            };
            items.push(entry);
            if items.len() > max_tool_events {
                let drop_n = items.len() - max_tool_events;
                items.drain(0..drop_n);
            }
        }
    }
}

/// Canonicalize JSON for deterministic blob generation.
///
/// Functional scope:
/// - Recursively sorts object keys alphabetically using a `BTreeMap` so that two
///   semantically equal payloads always serialise to the same byte sequence.
/// - Arrays preserve order; only object key order is normalised.
///
/// Boundary conditions:
/// - Numbers, strings, booleans, and nulls are returned untouched.
/// - The function is `O(n log n)` in total node count due to the per-object sort.
pub fn normalize_json_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_json_value).collect()),
        Value::Object(map) => {
            let normalized = map
                .into_iter()
                .map(|(key, value)| (key, normalize_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(normalized.into_iter().collect())
        }
        other => other,
    }
}

/// Build a [`LifecycleEvent`] from a parsed envelope of the given kind.
///
/// Functional scope: probes a small set of well-known field names in `envelope.extra`
/// (`prompt`/`message`/`user_prompt`, `tool_input`/`tool_request`,
/// `tool_response`/`tool_result`, etc.) so that providers using slightly different
/// vocabulary still flow through the same downstream pipeline.
///
/// Boundary conditions: any missing field is left as `None` so callers can detect
/// and ignore events that don't carry the expected payload.
pub(crate) fn build_lifecycle_event(
    kind: LifecycleEventKind,
    envelope: &SessionHookEnvelope,
) -> LifecycleEvent {
    LifecycleEvent {
        kind,
        session_id: envelope.session_id.clone(),
        // A provider-declared transcript location is only an untrusted
        // ingress hint.  It is deliberately not a lifecycle fact: keeping it
        // out of the canonical event prevents an arbitrary provider-home
        // pathname from reaching session state, event sidecars, or history.
        // Provenance-aware snapshot resolution derives a source separately
        // from the verified worktree and provider session id.
        session_ref: None,
        prompt: find_string(&envelope.extra, &["prompt", "message", "user_prompt"]),
        model: extract_model(&envelope.extra),
        source: envelope.extra.get("source").cloned(),
        tool_name: find_string(&envelope.extra, &["tool_name", "tool"]),
        tool_input: envelope
            .extra
            .get("tool_input")
            .cloned()
            .or_else(|| envelope.extra.get("tool_request").cloned()),
        tool_response: envelope
            .extra
            .get("tool_response")
            .cloned()
            .or_else(|| envelope.extra.get("tool_result").cloned()),
        assistant_message: find_string(
            &envelope.extra,
            &["last_assistant_message", "assistant_message", "message"],
        ),
        timestamp: Utc::now(),
    }
}

/// Reject session IDs that are too long or contain unsafe characters.
///
/// Functional scope: limits the alphabet to `[A-Za-z0-9._-]` and the length to 128
/// characters. The conservative alphabet keeps session IDs usable as filename
/// segments without per-platform escaping.
fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.len() > 128 {
        bail!("invalid session_id: exceeds 128 characters");
    }
    if !session_id
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || matches!(char, '.' | '_' | '-'))
    {
        bail!("invalid session_id: only [A-Za-z0-9._-] is allowed");
    }
    Ok(())
}

/// Verify the transcript path is plausible: non-empty, no NUL byte, bounded length.
///
/// Boundary conditions: the upper bound is configurable so embedding contexts that
/// know they only see short transcript paths can tighten the limit.
fn validate_transcript_path(transcript_path: &str, max_transcript_path_bytes: usize) -> Result<()> {
    if transcript_path.trim().is_empty() {
        bail!("invalid transcript_path: value cannot be empty");
    }
    if transcript_path.len() > max_transcript_path_bytes {
        bail!(
            "invalid transcript_path: exceeds {} bytes",
            max_transcript_path_bytes
        );
    }
    if transcript_path.contains('\0') {
        bail!("invalid transcript_path: contains NUL byte");
    }
    Ok(())
}

/// Return the first string value found among the listed keys, if any.
///
/// Used to fall back across renamed fields between provider versions without forcing
/// the rest of the parser to know each provider's vocabulary.
fn find_string(payload: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(Value::String(value)) = payload.get(*key) {
            return Some(value.clone());
        }
    }
    None
}

/// Locate the model identifier in either the top-level `model` field or inside the
/// nested `llm_request.model` object emitted by some providers.
fn extract_model(payload: &Map<String, Value>) -> Option<Value> {
    if let Some(model) = payload.get("model") {
        return Some(model.clone());
    }

    payload
        .get("llm_request")
        .and_then(Value::as_object)
        .and_then(|request| request.get("model"))
        .cloned()
}

/// Schema version stamped on every canonical lifecycle JSONL line
/// (`events/lifecycle.jsonl` inside an E4-libra checkpoint tree and the
/// session-level append-only log share this schema — E3-JSONL in
/// `docs/development/tracing/agent.md`).
///
/// v2 makes `identity_scheme` mandatory.  It is deliberately a per-line
/// field: an event UUID alone cannot say whether it is a native replay HMAC,
/// a fallback action HMAC, or one of the deterministic UUIDv5 forms.
pub const LIFECYCLE_EVENT_JSONL_SCHEMA_VERSION: u32 = 2;

/// The derivation class declared by a v2 canonical lifecycle line.
///
/// These strings are an external JSON contract.  They describe how the
/// writer obtained `event_id`; they do not themselves authenticate a stored
/// line.  In particular, a reader without the repository key cannot promote
/// a declared HMAC form into a verified replay receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleIdentityScheme {
    /// A provider-native scalar replay key was present and was converted to
    /// the repository-keyed `capture-dedup-v2` HMAC identity.
    NativeReplayHmacV2,
    /// No provider-native replay key was available; the action UUID comes
    /// from the repository-keyed `capture-event-v1` HMAC identity.
    FallbackActionHmacV1,
    /// The generic lifecycle serializer used `LifecycleEvent::event_id()`.
    GenericLifecycleUuidV5,
    /// Historical import derives one UUIDv5 per logical imported turn.
    ImportUuidV5,
}

impl LifecycleIdentityScheme {
    pub(crate) const fn as_wire(self) -> &'static str {
        match self {
            Self::NativeReplayHmacV2 => "native_replay_hmac_v2",
            Self::FallbackActionHmacV1 => "fallback_action_hmac_v1",
            Self::GenericLifecycleUuidV5 => "generic_lifecycle_uuid_v5",
            Self::ImportUuidV5 => "import_uuid_v5",
        }
    }

    #[cfg(test)]
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "native_replay_hmac_v2" => Some(Self::NativeReplayHmacV2),
            "fallback_action_hmac_v1" => Some(Self::FallbackActionHmacV1),
            "generic_lifecycle_uuid_v5" => Some(Self::GenericLifecycleUuidV5),
            "import_uuid_v5" => Some(Self::ImportUuidV5),
            _ => None,
        }
    }
}

/// The safe interpretation of a lifecycle line's identity declaration.
///
/// Legacy schema-v1 lines had no typed derivation field.  Their UUIDs are
/// intentionally opaque: even if a malformed v1 line carries a string named
/// `identity_scheme`, readers must not infer an HMAC/replay guarantee from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum ParsedLifecycleIdentityScheme {
    /// A v2 line explicitly declared one current writer scheme.  This is a
    /// classification, not independent cryptographic verification.
    Declared(LifecycleIdentityScheme),
    /// A schema-v1 line has no trustworthy derivation metadata.
    LegacyV1Opaque,
}

/// Classify the declared identity scheme of one canonical lifecycle line.
///
/// This deliberately performs only narrow schema interpretation.  It is not
/// a full lifecycle JSONL reader or a verifier for checkpoint/export/doctor;
/// those commands currently use object/manifest checks and must not treat a
/// line's `event_id` as a replay credential.
#[cfg(test)]
pub(crate) fn parse_canonical_lifecycle_identity_scheme(
    line: &Value,
) -> Result<ParsedLifecycleIdentityScheme> {
    let object = line
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("canonical lifecycle line must be a JSON object"))?;
    let schema_version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("canonical lifecycle line has no numeric schema_version"))?;

    // Do this before inspecting `identity_scheme`: v1 never defined that
    // field, so accepting an injected label would create phantom HMAC trust.
    if schema_version == 1 {
        return Ok(ParsedLifecycleIdentityScheme::LegacyV1Opaque);
    }
    if schema_version != u64::from(LIFECYCLE_EVENT_JSONL_SCHEMA_VERSION) {
        bail!("canonical lifecycle line has an unsupported schema version");
    }

    let identity_scheme = object
        .get("identity_scheme")
        .and_then(Value::as_str)
        .and_then(LifecycleIdentityScheme::from_wire)
        .ok_or_else(|| {
            anyhow::anyhow!("canonical lifecycle line has an invalid identity_scheme")
        })?;
    Ok(ParsedLifecycleIdentityScheme::Declared(identity_scheme))
}

/// Identity fields shared by every canonical lifecycle JSONL line for one
/// (session, ingest) context. Split out so multi-event batches serialise
/// with one context instead of repeating four arguments per event.
#[derive(Debug, Clone)]
pub struct CanonicalEventContext<'a> {
    /// `agent_session.agent_kind` snake_case tag (`AgentKind::as_db_str`).
    pub agent_kind: &'a str,
    /// Libra's namespaced session id (`<provider>__<provider_session_id>`).
    pub session_id: &'a str,
    /// The provider's native session id, preserved verbatim.
    pub provider_session_id: &'a str,
    /// Mandatory v2 declaration of the derivation used for every event in
    /// this writer context.  Keeping it typed prevents a call site from
    /// silently omitting or free-forming the external wire value.
    pub identity_scheme: LifecycleIdentityScheme,
    /// Free-form provenance object (e.g. `{"channel":"hook",
    /// "hook_event_name":"Stop"}`). Never carries raw envelope payload.
    pub provenance: Value,
}

/// Serialise one **already-redacted** [`LifecycleEvent`] into the canonical
/// E3-JSONL object (`schema_version`, `event_id`, `identity_scheme`, `kind`,
/// `agent_kind`, `session_id`, `provider_session_id`, `timestamp`, `source`,
/// `partial`, `provenance` + per-kind optional fields).
///
/// The caller owns redaction: this function performs none, so it must only
/// ever see events that already passed the ingest redaction pass.
pub fn lifecycle_event_canonical_json(
    event: &LifecycleEvent,
    ctx: &CanonicalEventContext,
) -> Value {
    lifecycle_event_canonical_json_with_identity(event, ctx, event.event_id(), false)
}

/// Canonical serializer variant for synthesized provider-neutral events whose
/// stable identity and partial status come from an external replay contract
/// rather than a hook envelope timestamp.
pub(crate) fn lifecycle_event_canonical_json_with_identity(
    event: &LifecycleEvent,
    ctx: &CanonicalEventContext,
    event_id: Uuid,
    partial: bool,
) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "schema_version".to_string(),
        json!(LIFECYCLE_EVENT_JSONL_SCHEMA_VERSION),
    );
    obj.insert("event_id".to_string(), json!(event_id.to_string()));
    obj.insert(
        "identity_scheme".to_string(),
        json!(ctx.identity_scheme.as_wire()),
    );
    obj.insert("kind".to_string(), json!(event.event_kind()));
    obj.insert("agent_kind".to_string(), json!(ctx.agent_kind));
    obj.insert("session_id".to_string(), json!(ctx.session_id));
    obj.insert(
        "provider_session_id".to_string(),
        json!(ctx.provider_session_id),
    );
    obj.insert("timestamp".to_string(), json!(event.timestamp.to_rfc3339()));
    obj.insert(
        "source".to_string(),
        event.source.clone().unwrap_or(Value::Null),
    );
    // Hook events pass false; historical replay uses true for a growing final
    // turn so its canonical sidecar agrees with the coverage claim.
    obj.insert("partial".to_string(), json!(partial));
    obj.insert("provenance".to_string(), ctx.provenance.clone());

    // Per-kind optional fields, present only when the event carries them.
    if let Some(prompt) = &event.prompt {
        obj.insert("prompt".to_string(), json!(prompt));
    }
    if let Some(model) = &event.model {
        obj.insert("model".to_string(), model.clone());
    }
    if let Some(tool_name) = &event.tool_name {
        obj.insert("tool_name".to_string(), json!(tool_name));
    }
    if let Some(tool_input) = &event.tool_input {
        obj.insert("tool_input".to_string(), tool_input.clone());
    }
    if let Some(tool_response) = &event.tool_response {
        obj.insert("tool_response".to_string(), tool_response.clone());
    }
    if let Some(message) = &event.assistant_message {
        obj.insert("assistant_message".to_string(), json!(message));
    }
    // `session_ref` is deliberately excluded. It historically carried a
    // provider-supplied transcript path, which is a provenance input rather
    // than syncable lifecycle evidence.
    Value::Object(obj)
}

/// Serialise a batch of already-redacted lifecycle events as canonical
/// JSONL bytes — one [`lifecycle_event_canonical_json`] object per line,
/// each line newline-terminated. This is the byte stream the E4-libra
/// checkpoint writer lands at `events/lifecycle.jsonl`; today's writer
/// passes the single triggering event, but the slice signature keeps
/// multi-event batches (e.g. buffered turn replays) source-compatible.
pub fn lifecycle_events_to_canonical_jsonl(
    events: &[&LifecycleEvent],
    ctx: &CanonicalEventContext,
) -> Vec<u8> {
    let mut out = Vec::new();
    for event in events {
        let line = lifecycle_event_canonical_json(event, ctx);
        // A `serde_json::Value` never fails to serialise; fall back to an
        // empty object rather than propagating an impossible error.
        let serialized = serde_json::to_vec(&line).unwrap_or_else(|_| b"{}".to_vec());
        out.extend_from_slice(&serialized);
        out.push(b'\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Scenario: each lifecycle kind formats as the documented snake_case string.
    #[test]
    fn lifecycle_event_kind_display() {
        assert_eq!(
            LifecycleEventKind::SessionStart.to_string(),
            "session_start"
        );
        assert_eq!(LifecycleEventKind::TurnStart.to_string(), "turn_start");
        assert_eq!(LifecycleEventKind::ToolUse.to_string(), "tool_use");
        assert_eq!(LifecycleEventKind::ModelUpdate.to_string(), "model_update");
        assert_eq!(LifecycleEventKind::Compaction.to_string(), "compaction");
        assert_eq!(
            LifecycleEventKind::CompactionCompleted.to_string(),
            "compaction_completed"
        );
        assert_eq!(
            LifecycleEventKind::PermissionRequest.to_string(),
            "permission_request"
        );
        assert_eq!(
            LifecycleEventKind::SourceEnabled.to_string(),
            "source_enabled"
        );
        assert_eq!(
            LifecycleEventKind::SourceDisabled.to_string(),
            "source_disabled"
        );
        assert_eq!(LifecycleEventKind::TurnEnd.to_string(), "turn_end");
        assert_eq!(LifecycleEventKind::SessionEnd.to_string(), "session_end");
        assert_eq!(
            LifecycleEventKind::SubagentStart.to_string(),
            "subagent_start"
        );
        assert_eq!(LifecycleEventKind::SubagentEnd.to_string(), "subagent_end");
    }

    /// AG-19 ordinal-stability pin: `event_id()` folds `kind as u8` into
    /// its UUIDv5 name, so the discriminants of PRE-EXISTING variants are
    /// part of the persisted-id contract. New variants must be appended
    /// (SubagentStart=11, SubagentEnd=12) — inserting one mid-enum shifts
    /// every later ordinal and silently renumbers persisted event ids.
    #[test]
    fn lifecycle_event_kind_ordinals_are_stable() {
        for (kind, ordinal) in [
            (LifecycleEventKind::SessionStart, 0u8),
            (LifecycleEventKind::TurnStart, 1),
            (LifecycleEventKind::ToolUse, 2),
            (LifecycleEventKind::ModelUpdate, 3),
            (LifecycleEventKind::Compaction, 4),
            (LifecycleEventKind::CompactionCompleted, 5),
            (LifecycleEventKind::PermissionRequest, 6),
            (LifecycleEventKind::SourceEnabled, 7),
            (LifecycleEventKind::SourceDisabled, 8),
            (LifecycleEventKind::TurnEnd, 9),
            (LifecycleEventKind::SessionEnd, 10),
            (LifecycleEventKind::SubagentStart, 11),
            (LifecycleEventKind::SubagentEnd, 12),
        ] {
            assert_eq!(kind as u8, ordinal, "{kind:?} discriminant drifted");
        }
    }

    // Scenario: a path-traversal style session ID is rejected by the validator.
    #[test]
    fn validate_envelope_rejects_bad_session_id() {
        let envelope = SessionHookEnvelope {
            hook_event_name: "SessionStart".to_string(),
            session_id: "../bad".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: None,
            extra: Map::new(),
        };
        assert!(validate_session_hook_envelope(&envelope, 4096).is_err());
    }

    // Scenario: object keys are sorted recursively so canonical JSON is stable.
    #[test]
    fn normalize_value_sorts_object_keys() {
        let value = json!({
            "z": 1,
            "a": {
                "k2": 2,
                "k1": 1
            }
        });

        let canonical = serde_json::to_string(&normalize_json_value(value)).unwrap();
        assert_eq!(canonical, r#"{"a":{"k1":1,"k2":2},"z":1}"#);
    }

    // Scenario: a transcript path containing a NUL byte is rejected as unsafe.
    #[test]
    fn validate_envelope_rejects_invalid_transcript_path() {
        let envelope = SessionHookEnvelope {
            hook_event_name: "SessionStart".to_string(),
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: Some("\0bad".to_string()),
            extra: Map::new(),
        };
        assert!(validate_session_hook_envelope(&envelope, 4096).is_err());
    }

    // Scenario: a transcript path that's only whitespace is treated as empty.
    #[test]
    fn validate_envelope_rejects_empty_transcript_path() {
        let envelope = SessionHookEnvelope {
            hook_event_name: "SessionStart".to_string(),
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: Some("   ".to_string()),
            extra: Map::new(),
        };
        assert!(validate_session_hook_envelope(&envelope, 4096).is_err());
    }

    // -------------------------------------------------------------------
    // AG-20: canonical E3-JSONL serialisation for events/lifecycle.jsonl
    // -------------------------------------------------------------------

    fn canonical_test_event(kind: LifecycleEventKind) -> LifecycleEvent {
        LifecycleEvent {
            kind,
            session_id: "provider-sess-1".to_string(),
            session_ref: Some("/home/u/.claude/t.jsonl".to_string()),
            prompt: Some("redacted prompt".to_string()),
            model: Some(json!("claude-sonnet-4-5")),
            source: Some(json!("startup")),
            tool_name: None,
            tool_input: None,
            tool_response: None,
            assistant_message: None,
            timestamp: DateTime::parse_from_rfc3339("2026-07-05T01:02:03Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    fn canonical_test_ctx() -> CanonicalEventContext<'static> {
        CanonicalEventContext {
            agent_kind: "claude_code",
            session_id: "claude__provider-sess-1",
            provider_session_id: "provider-sess-1",
            identity_scheme: LifecycleIdentityScheme::GenericLifecycleUuidV5,
            provenance: json!({"channel": "hook", "hook_event_name": "Stop"}),
        }
    }

    /// Every canonical line carries the E3-JSONL required keys with the
    /// pinned wire spellings, plus the per-kind optional fields present on
    /// the event.
    #[test]
    fn canonical_event_json_carries_required_e3_fields() {
        let event = canonical_test_event(LifecycleEventKind::TurnEnd);
        let line = lifecycle_event_canonical_json(&event, &canonical_test_ctx());

        assert_eq!(
            line["schema_version"],
            json!(LIFECYCLE_EVENT_JSONL_SCHEMA_VERSION)
        );
        assert_eq!(line["kind"], json!("turn_end"));
        assert_eq!(line["agent_kind"], json!("claude_code"));
        assert_eq!(line["session_id"], json!("claude__provider-sess-1"));
        assert_eq!(line["provider_session_id"], json!("provider-sess-1"));
        assert_eq!(line["identity_scheme"], json!("generic_lifecycle_uuid_v5"));
        assert_eq!(line["partial"], json!(false));
        assert_eq!(line["provenance"]["channel"], json!("hook"));
        assert_eq!(line["source"], json!("startup"));
        assert_eq!(line["prompt"], json!("redacted prompt"));
        assert_eq!(line["model"], json!("claude-sonnet-4-5"));
        // event_id is the deterministic v5 UUID from the event identity.
        assert_eq!(
            line["event_id"],
            json!(event.event_id().to_string()),
            "event_id must match the Event-trait derivation"
        );
        // RFC3339 timestamp.
        let ts = line["timestamp"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(ts).expect("timestamp must be RFC3339");
        // Absent optional fields stay absent (no null noise).
        assert!(line.get("tool_name").is_none());
        assert!(line.get("assistant_message").is_none());
    }

    /// Batch serialisation writes exactly one newline-terminated JSON
    /// object per event, in order.
    #[test]
    fn canonical_jsonl_is_one_line_per_event_in_order() {
        let start = canonical_test_event(LifecycleEventKind::SessionStart);
        let end = canonical_test_event(LifecycleEventKind::SessionEnd);
        let ctx = canonical_test_ctx();
        let bytes = lifecycle_events_to_canonical_jsonl(&[&start, &end], &ctx);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.ends_with('\n'), "JSONL must be newline-terminated");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["kind"], json!("session_start"));
        assert_eq!(second["kind"], json!("session_end"));
    }

    /// Schema-v2 writers always emit a fixed typed identity declaration, and
    /// the narrow reader preserves that declaration without claiming to
    /// independently verify its HMAC material.
    #[test]
    fn canonical_v2_identity_scheme_is_typed_and_mandatory() {
        let event = canonical_test_event(LifecycleEventKind::SessionStart);
        let line = lifecycle_event_canonical_json(&event, &canonical_test_ctx());
        assert_eq!(
            parse_canonical_lifecycle_identity_scheme(&line).expect("parse v2 declaration"),
            ParsedLifecycleIdentityScheme::Declared(
                LifecycleIdentityScheme::GenericLifecycleUuidV5
            )
        );

        let mut missing_scheme = line;
        missing_scheme
            .as_object_mut()
            .expect("canonical line is an object")
            .remove("identity_scheme");
        assert!(
            parse_canonical_lifecycle_identity_scheme(&missing_scheme).is_err(),
            "schema-v2 lines must not silently omit their identity scheme"
        );
    }

    /// v1 did not define `identity_scheme`. Even a hostile or hand-edited
    /// v1 record which adds the current HMAC spelling remains opaque, so no
    /// consumer can manufacture replay trust from historical data.
    #[test]
    fn legacy_v1_identity_scheme_is_always_opaque_not_hmac() {
        let legacy = json!({
            "schema_version": 1,
            "event_id": "4de63a6b-8a12-4f77-9c4b-050206011005",
            "identity_scheme": "native_replay_hmac_v2",
        });
        assert_eq!(
            parse_canonical_lifecycle_identity_scheme(&legacy)
                .expect("legacy schema must remain parseable"),
            ParsedLifecycleIdentityScheme::LegacyV1Opaque,
        );
    }

    #[test]
    fn lifecycle_identity_scheme_wire_values_are_pinned() {
        assert_eq!(
            LifecycleIdentityScheme::NativeReplayHmacV2.as_wire(),
            "native_replay_hmac_v2"
        );
        assert_eq!(
            LifecycleIdentityScheme::FallbackActionHmacV1.as_wire(),
            "fallback_action_hmac_v1"
        );
        assert_eq!(
            LifecycleIdentityScheme::GenericLifecycleUuidV5.as_wire(),
            "generic_lifecycle_uuid_v5"
        );
        assert_eq!(
            LifecycleIdentityScheme::ImportUuidV5.as_wire(),
            "import_uuid_v5"
        );
    }
}
