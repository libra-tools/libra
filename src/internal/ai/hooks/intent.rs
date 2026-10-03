//! Legacy `AiIntent` hook writer (`HookTarget::AiIntent`).
//!
//! Persists a command that capture ingress already validated and lowered into
//! the legacy agent session store and, on `SessionEnd`, materializes the
//! redacted `ai_session` blob into the AI history ref. Installed hook
//! configurations use the AgentTraces target; this public compatibility path
//! stays callable by library users and never parses raw hook data. The shared
//! hook runtime (`hooks::runtime`) dispatches here only after ingress succeeds.

use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde_json::{Value, json};

use super::{
    lifecycle::{
        LifecycleEvent, LifecycleEventKind, SessionHookEnvelope, apply_lifecycle_event,
        normalize_json_value,
    },
    provider::HookProvider,
};
use crate::{
    internal::{
        ai::{
            automation::dispatch_repo_hook_lifecycle_event_to_history,
            capture::{
                ingress::CaptureIngressCommand,
                live::{
                    build_ai_session_id, narrow_envelope_from_capture_context, redact_session_id,
                    set_hash_kind_from_connection,
                },
            },
            history,
            session::{SessionState, SessionStore},
        },
        db,
    },
    utils::{client_storage::ClientStorage, error::emit_warning, object::write_git_object, util},
};

/// Persist a command already canonicalized at the hook boundary to the legacy
/// AI intent session store. This function must never parse or validate raw hook
/// data: target dispatch happens only after [`CaptureIngressCommand`] succeeds.
pub(super) async fn process_ai_intent_ingress(
    ingress_command: Box<CaptureIngressCommand>,
    provider: &dyn HookProvider,
) -> Result<()> {
    let crate::internal::ai::capture::ingress::CaptureIngressParts {
        hook_command: command,
        provider_kind,
        dedup_key,
        context,
        event,
        ..
    } = (*ingress_command).into_parts();
    debug_assert_eq!(provider_kind, provider.provider_name());
    let (envelope, runtime_scope) = narrow_envelope_from_capture_context(context, event.kind);

    let process_cwd = runtime_scope.worktree_root;
    let storage_path = runtime_scope.storage_path;
    let config_conn = db::get_db_conn_instance_for_path(&storage_path.join(util::DATABASE))
        .await
        .map_err(|err| anyhow!("failed to open libra database for hook configuration: {err}"))?;
    set_hash_kind_from_connection(&config_conn)
        .await
        .context("failed to configure hash kind from repo config")?;

    let process_cwd_str = process_cwd.to_string_lossy().to_string();
    // CEX-EntireIO §11.2: agent capture sessions live under `sessions/agent/`
    // so their session-id locks cannot collide with `libra code` session
    // locks (which still live one level up at `sessions/`). The store also
    // adopts any in-flight legacy entry, preserving hook continuity for
    // sessions that started before this partition existed.
    let session_store = SessionStore::from_storage_path_with_subdir(&storage_path, "agent");

    let ai_session_id = build_ai_session_id(provider.provider_name(), &envelope.session_id);
    if session_store
        .adopt_legacy_subdir_session_if_needed(&ai_session_id)
        .is_err()
    {
        tracing::warn!(
            session_id = %redact_session_id(&ai_session_id),
            reason = "legacy_session_subdir_adoption_failed",
            "failed to migrate legacy session into agent subdir; continuing with fresh session under sessions/agent/"
        );
    }
    let recovered_from_out_of_order = event.kind != LifecycleEventKind::SessionStart;
    let _session_lock = session_store
        .lock_session(&ai_session_id)
        .with_context(|| {
            format!(
                "failed to acquire session lock for '{}'",
                redact_session_id(&ai_session_id)
            )
        })?;

    let mut session = match session_store.load(&ai_session_id) {
        Ok(session) => session,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let mut recovered = SessionState::new(&process_cwd_str);
            recovered.id = ai_session_id.clone();
            recovered.working_dir = process_cwd_str.clone();
            if recovered_from_out_of_order {
                recovered
                    .metadata
                    .insert("recovered_from_out_of_order".to_string(), json!(true));
            }
            recovered
        }
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            let corrupt_backup_archived = match session_store
                .archive_corrupt_session(&ai_session_id)
            {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(_) => {
                    eprintln!(
                        "warning: failed to archive malformed session cache; continuing with a new in-memory session"
                    );
                    false
                }
            };
            eprintln!(
                "warning: malformed session cache detected; recovering with a new in-memory session"
            );

            let mut recovered = SessionState::new(&process_cwd_str);
            recovered.id = ai_session_id.clone();
            recovered.working_dir = process_cwd_str.clone();
            recovered
                .metadata
                .insert("recovered_from_corrupt_session".to_string(), json!(true));
            recovered.metadata.insert(
                "recovery_error".to_string(),
                json!("malformed_session_cache"),
            );
            if corrupt_backup_archived {
                recovered
                    .metadata
                    .insert("corrupt_session_backup_archived".to_string(), json!(true));
            }
            recovered
        }
        Err(_) => {
            return Err(anyhow!(
                "failed to load the local session cache; retry the hook or repair the local cache"
            ));
        }
    };

    session.id = ai_session_id;
    session.working_dir = process_cwd_str.clone();
    session.metadata.insert(
        PROVIDER_METADATA_KEY.to_string(),
        json!(provider.provider_name().to_string()),
    );
    session.metadata.insert(
        PROVIDER_SESSION_ID_METADATA_KEY.to_string(),
        json!(envelope.session_id.clone()),
    );

    if envelope.cwd != process_cwd_str {
        // The verified hook cwd need not match the caller's current subdir,
        // but retaining another path spelling in durable legacy state is not
        // needed for projection and would widen the hook-data surface.
        session
            .metadata
            .insert("hook_cwd_mismatch".to_string(), json!(true));
    } else {
        session.metadata.remove("hook_cwd_mismatch");
    }
    session.metadata.remove("hook_reported_cwd");

    if dedup_hit(&session, dedup_key.as_deref()) {
        if event.kind != LifecycleEventKind::SessionEnd {
            return Ok(());
        }
        if session_persisted(&session) {
            return Ok(());
        }
    }

    // This public compatibility entry is not used by installed hook configs,
    // but it remains callable by library users. Do not let its legacy intent
    // projection become an escape hatch for raw provider input: redact every
    // event value and drop arbitrary envelope extras before any session JSON
    // or history object is built.
    let (envelope, event) = prepare_ai_intent_persistence(envelope, event)
        .context("redact legacy AI intent hook projection")?;
    sanitize_legacy_ai_intent_session(&mut session)
        .context("sanitize legacy AI intent session state")?;

    apply_hook_event(&mut session, &envelope, &event, provider.provider_name());
    provider
        .post_process_event(command, &storage_path, &mut session, &envelope, &event)
        .context("provider hook post-processing failed")?;
    if let Some(event_key) = dedup_key {
        append_processed_event_key(&mut session, event_key);
    }

    if dispatch_repo_hook_lifecycle_event_to_history(&process_cwd, &storage_path, event.kind)
        .await
        .is_err()
    {
        emit_warning("failed to dispatch automation hook event");
    }

    if event.kind == LifecycleEventKind::SessionEnd {
        match persist_session_history(&storage_path, &session, provider, &config_conn).await {
            Ok(outcome) => {
                session
                    .metadata
                    .insert("persisted".to_string(), json!(true));
                session
                    .metadata
                    .insert("persisted_at".to_string(), json!(Utc::now().to_rfc3339()));
                session
                    .metadata
                    .insert("history_ref".to_string(), json!(history::ai_ref_name()));
                session
                    .metadata
                    .insert("object_hash".to_string(), json!(outcome.object_hash));
                session.metadata.insert(
                    "persisted_from_history".to_string(),
                    json!(outcome.already_exists),
                );
                session.metadata.remove("persist_failed");
                session.metadata.remove("cleanup_failed");
                session.metadata.remove("last_error");

                match session_store.delete(&session.id) {
                    Ok(_) => return Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(_) => {
                        session
                            .metadata
                            .insert("cleanup_failed".to_string(), json!(true));
                        session.metadata.insert(
                            "last_error".to_string(),
                            json!("session_cache_cleanup_failed"),
                        );
                    }
                }
            }
            Err(_) => {
                session
                    .metadata
                    .insert("persist_failed".to_string(), json!(true));
                session.metadata.insert(
                    "last_error".to_string(),
                    json!("session_history_persistence_failed"),
                );
                session
                    .metadata
                    .insert("persisted".to_string(), json!(false));
                emit_warning("failed to persist session history");
                session_store.save(&session).map_err(|_| {
                    anyhow!(
                        "failed to save the retryable local session cache after persistence failure"
                    )
                })?;
                return Err(anyhow!(
                    "session history persistence failed; retry the hook or inspect the local repository"
                ));
            }
        }
    }

    session_store.save(&session).map_err(|_| {
        anyhow!(
            "failed to save the local session cache; retry the hook or inspect the local repository"
        )
    })?;
    Ok(())
}

// Metadata keys persisted on `SessionState`. Centralised here so that ingestion,
// projection, and tests all see the same names.
const PROCESSED_EVENT_KEYS: &str = "processed_event_keys";
const NORMALIZED_EVENTS_KEY: &str = "normalized_events";
const PROVIDER_METADATA_KEY: &str = "provider";
const PROVIDER_SESSION_ID_METADATA_KEY: &str = "provider_session_id";
const SESSION_PHASE_METADATA_KEY: &str = "session_phase";

// Resource bounds. The values are deliberately small enough to stay in memory for
// the longest plausible session while large enough to capture the events the agent
// actually needs for projection.
const MAX_PROCESSED_EVENT_KEYS: usize = 200;
const MAX_NORMALIZED_EVENTS: usize = 400;
const MAX_TOOL_EVENTS: usize = 200;

/// Object type tag stamped on persisted AI session blobs.
pub const AI_SESSION_TYPE: &str = "ai_session";
/// Schema version. Bump when the persisted shape changes incompatibly.
pub const AI_SESSION_SCHEMA: &str = "libra.ai_session.v2";

/// Coarse session lifecycle phase recorded as `session_phase` metadata.
///
/// Distinct from [`LifecycleEventKind`] — the latter is per-event, the former is
/// aggregated state suitable for UIs (a single status badge per session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    Active,
    Stopped,
    Ended,
}

/// Outcome of attempting to persist a session at SessionEnd.
///
/// Carries the resulting blob's object hash so callers can advertise it on the
/// session's metadata, and `already_exists` to distinguish a fresh write from a
/// retry that reused a previous blob (idempotent SessionEnd handling).
#[derive(Debug)]
struct PersistOutcome {
    object_hash: String,
    already_exists: bool,
}

/// Redact a scalar string before it can reach the legacy AI-intent store.
fn redact_ai_intent_string(
    value: &mut String,
    redactor: &crate::internal::ai::observed_agents::Redactor,
) {
    let (redacted, _) = redactor.redact(value.as_bytes());
    *value = String::from_utf8_lossy(redacted.bytes()).into_owned();
}

/// Redact a JSON value, including object keys, before it can reach durable
/// legacy state. The temporary serialization remains inside the ingress stack;
/// a failure to reconstruct valid redacted JSON is fail-closed rather than
/// silently retaining any source bytes.
fn redact_ai_intent_json(
    value: &mut Value,
    redactor: &crate::internal::ai::observed_agents::Redactor,
) -> Result<()> {
    let raw = serde_json::to_vec(value).context("serialize legacy AI intent JSON for redaction")?;
    let (redacted, _) = redactor.redact(&raw);
    *value = serde_json::from_slice(redacted.bytes())
        .context("decode redacted legacy AI intent JSON")?;
    Ok(())
}

/// Project ingress data onto the narrow legacy AI-intent persistence contract.
///
/// Installed hook configurations use `AgentTraces`; this compatibility path
/// remains available to library callers. It must not turn into a raw-envelope
/// side channel: provider extras and transcript paths are deliberately dropped,
/// while the handful of canonical lifecycle fields are redacted before they can
/// affect session JSON, history blobs, or a provider post-processing hook.
fn prepare_ai_intent_persistence(
    mut envelope: SessionHookEnvelope,
    mut event: LifecycleEvent,
) -> Result<(SessionHookEnvelope, LifecycleEvent)> {
    use crate::internal::ai::observed_agents::Redactor;

    let redactor = Redactor::new_default();
    for value in [
        &mut event.prompt,
        &mut event.tool_name,
        &mut event.assistant_message,
    ]
    .into_iter()
    .flatten()
    {
        redact_ai_intent_string(value, &redactor);
    }
    // Structured provider values are not part of the legacy intent contract.
    // Even a redacted JSON object retains attacker-controlled key structure,
    // so omit it rather than turn the compatibility path into a second raw
    // payload persistence format. A scalar model label remains useful and is
    // safely redacted; every other model/source shape is dropped.
    match event.model.as_mut() {
        Some(Value::String(model)) => redact_ai_intent_string(model, &redactor),
        Some(_) => event.model = None,
        None => {}
    }
    event.source = None;
    event.tool_input = None;
    event.tool_response = None;
    // The transcript path is a provider-supplied capability, not a legacy
    // intent-history field. Dropping both spellings prevents the raw envelope
    // from reappearing through `apply_hook_event` or a provider extension.
    envelope.transcript_path = None;
    envelope.extra.clear();
    event.session_ref = None;
    Ok((envelope, event))
}

/// Remove raw-envelope remnants from a legacy session loaded before the
/// ingress boundary existed, then redact any retained free-form state before
/// it can be re-saved or materialized into an immutable history blob.
fn sanitize_legacy_ai_intent_session(session: &mut SessionState) -> Result<()> {
    use crate::internal::ai::observed_agents::Redactor;

    let redactor = Redactor::new_default();
    session.metadata.remove("raw_hook_events");
    session.metadata.remove("hook_reported_cwd");
    // A pre-ingress session cache may carry the former raw provider locator.
    // It is neither a portable history field nor an authorized snapshot source;
    // delete it before a legacy SessionEnd materializes immutable history.
    session.metadata.remove("transcript_path");
    redact_ai_intent_string(&mut session.summary, &redactor);
    for message in &mut session.messages {
        redact_ai_intent_string(&mut message.content, &redactor);
    }
    for value in session.metadata.values_mut() {
        redact_ai_intent_json(value, &redactor)?;
    }
    Ok(())
}

/// Apply the canonical event together with bookkeeping into `session`.
///
/// Functional scope: bumps `updated_at`, applies the lifecycle delta, and
/// transitions the coarse phase. Finally appends a redacted normalized
/// projection-friendly fragment to `normalized_events`; raw envelopes never
/// enter session metadata.
fn apply_hook_event(
    session: &mut SessionState,
    envelope: &SessionHookEnvelope,
    event: &LifecycleEvent,
    provider_name: &str,
) {
    session.updated_at = Utc::now();

    // Old session files may predate the ingress boundary. Never carry their
    // raw ring forward when a later lifecycle event reopens the session.
    session.metadata.remove("raw_hook_events");
    apply_lifecycle_event(session, event, MAX_TOOL_EVENTS);
    transition_phase(session, event.kind);
    append_normalized_event_with_envelope(session, event, provider_name, Some(envelope));
}

/// Compute the new [`SessionPhase`] given the previous phase and the incoming
/// event kind, then record it back on the session.
///
/// Functional scope: `SessionEnd` always wins, transitioning to `Ended`; any
/// activity event resets to `Active`; `TurnEnd` parks at `Stopped`; `ModelUpdate`
/// is a no-op preserving the current phase. This produces a small, deterministic
/// state machine usable as a UI badge.
fn transition_phase(session: &mut SessionState, event_kind: LifecycleEventKind) {
    let current_phase = session
        .metadata
        .get(SESSION_PHASE_METADATA_KEY)
        .and_then(Value::as_str)
        .and_then(|phase| match phase {
            "active" => Some(SessionPhase::Active),
            "stopped" => Some(SessionPhase::Stopped),
            "ended" => Some(SessionPhase::Ended),
            _ => None,
        });

    let next_phase = match event_kind {
        LifecycleEventKind::SessionEnd => SessionPhase::Ended,
        LifecycleEventKind::TurnEnd => SessionPhase::Stopped,
        LifecycleEventKind::SessionStart
        | LifecycleEventKind::TurnStart
        | LifecycleEventKind::ToolUse
        | LifecycleEventKind::Compaction
        | LifecycleEventKind::CompactionCompleted
        | LifecycleEventKind::PermissionRequest
        | LifecycleEventKind::SourceEnabled
        | LifecycleEventKind::SourceDisabled
        // AG-19: nested sub-agent activity keeps the parent session live.
        | LifecycleEventKind::SubagentStart
        | LifecycleEventKind::SubagentEnd => SessionPhase::Active,
        LifecycleEventKind::ModelUpdate => current_phase.unwrap_or(SessionPhase::Active),
    };

    session.metadata.insert(
        SESSION_PHASE_METADATA_KEY.to_string(),
        json!(next_phase.as_str()),
    );
}

/// Append a small projection-friendly summary of the event.
///
/// Functional scope: includes the kind, timestamp, prompt, tool name, assistant
/// message, and a few `has_*` flags so projections can render activity feeds
/// without paying the cost of streaming every raw envelope.
///
/// Boundary conditions: capped at `MAX_NORMALIZED_EVENTS`; oldest entries are
/// dropped first.
#[cfg(test)]
pub(crate) fn append_normalized_event(
    session: &mut SessionState,
    event: &LifecycleEvent,
    provider_name: &str,
) {
    append_normalized_event_with_envelope(session, event, provider_name, None);
}

/// Append a normalized event while retaining the provider-native correlation
/// fields that are not part of [`LifecycleEvent`]. Hook envelopes carry these
/// fields (`turn_id`, `tool_use_id`, sub-agent identity, compaction trigger,
/// and permission mode), and preserving them makes the event stream useful for
/// reconstructing an interaction without re-reading the unstable transcript.
fn append_normalized_event_with_envelope(
    session: &mut SessionState,
    event: &LifecycleEvent,
    provider_name: &str,
    envelope: Option<&SessionHookEnvelope>,
) {
    let entry = session
        .metadata
        .entry(NORMALIZED_EVENTS_KEY.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));

    let extra = envelope.map(|value| &value.extra);

    let normalized = json!({
        "provider": provider_name,
        "provider_session_id": envelope.map(|value| value.session_id.clone()),
        "kind": event.kind.to_string(),
        "timestamp": event.timestamp.to_rfc3339(),
        "prompt": event.prompt,
        "tool_name": event.tool_name,
        "tool_input": event.tool_input,
        "tool_response": event.tool_response,
        "assistant_message": event.assistant_message,
        "turn_id": extra.and_then(|values| values.get("turn_id")).cloned(),
        "tool_use_id": extra
            .and_then(|values| values.get("tool_use_id"))
            .cloned(),
        "agent_id": extra.and_then(|values| values.get("agent_id")).cloned(),
        "agent_type": extra
            .and_then(|values| values.get("agent_type"))
            .cloned(),
        "trigger": extra.and_then(|values| values.get("trigger")).cloned(),
        "permission_mode": extra
            .and_then(|values| values.get("permission_mode"))
            .cloned(),
        "session_end_reason": extra.and_then(|values| values.get("reason")).cloned(),
        "has_model": event.model.is_some(),
        "has_tool_input": event.tool_input.is_some(),
        "has_tool_response": event.tool_response.is_some(),
    });

    let Value::Array(items) = entry else {
        session.metadata.insert(
            NORMALIZED_EVENTS_KEY.to_string(),
            Value::Array(vec![normalized]),
        );
        return;
    };

    items.push(normalized);
    if items.len() > MAX_NORMALIZED_EVENTS {
        let drop_n = items.len() - MAX_NORMALIZED_EVENTS;
        items.drain(0..drop_n);
    }
}

/// Return true when `key` is already in the processed-keys ring.
///
/// Boundary conditions: a `None` key always returns false because callers asked
/// for "no dedup".
fn dedup_hit(session: &SessionState, key: Option<&str>) -> bool {
    let Some(key) = key else {
        return false;
    };
    session
        .metadata
        .get(PROCESSED_EVENT_KEYS)
        .and_then(Value::as_array)
        .map(|items| items.iter().any(|value| value.as_str() == Some(key)))
        .unwrap_or(false)
}

/// Push `key` onto the processed-keys ring, evicting old entries past
/// `MAX_PROCESSED_EVENT_KEYS`. The same defensive overwrite pattern as
/// [`append_normalized_event`] applies when the slot is the wrong shape.
fn append_processed_event_key(session: &mut SessionState, key: String) {
    let entry = session
        .metadata
        .entry(PROCESSED_EVENT_KEYS.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));

    let Value::Array(items) = entry else {
        session.metadata.insert(
            PROCESSED_EVENT_KEYS.to_string(),
            Value::Array(vec![json!(key)]),
        );
        return;
    };

    items.push(Value::String(key));
    if items.len() > MAX_PROCESSED_EVENT_KEYS {
        let drop_n = items.len() - MAX_PROCESSED_EVENT_KEYS;
        items.drain(0..drop_n);
    }
}

/// Whether the session has already been written to the AI history ref.
///
/// Used together with `dedup_hit` so a duplicate `SessionEnd` doesn't repeat the
/// blob write but still updates metadata fields that may have changed.
fn session_persisted(session: &SessionState) -> bool {
    session
        .metadata
        .get("persisted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Materialise the final session as a Git blob and append it to the AI history.
///
/// Functional scope:
/// - If a blob already exists for this session ID under [`AI_SESSION_TYPE`], reuse
///   its hash without writing a new one (idempotent).
/// - Otherwise serialise [`build_ai_session_payload`], write a Git blob, and
///   append a `(type, id, hash)` triple to the AI history ref.
///
/// Boundary conditions: any I/O error short-circuits; the caller retains only
/// a fixed safe reason in session metadata and surfaces an actionable retry
/// message without rendering filesystem paths or untrusted input.
async fn persist_session_history(
    storage_path: &Path,
    session: &SessionState,
    provider: &dyn HookProvider,
    db_conn: &sea_orm::DatabaseConnection,
) -> Result<PersistOutcome> {
    let objects_dir = storage_path.join("objects");
    std::fs::create_dir_all(&objects_dir)?;

    let storage = Arc::new(ClientStorage::init(objects_dir));
    let db_conn = Arc::new(db_conn.clone());
    let payload = build_ai_session_payload(session, provider);
    let blob_data = serde_json::to_vec(&normalize_json_value(payload))
        .context("failed to serialize ai_session payload")?;
    let blob_hash = write_git_object(storage_path, "blob", &blob_data)?;
    let (object_hash, already_exists) = history::persist_ai_session(
        storage,
        storage_path.to_path_buf(),
        db_conn,
        AI_SESSION_TYPE,
        &session.id,
        blob_hash,
    )
    .await?;

    Ok(PersistOutcome {
        object_hash: object_hash.to_string(),
        already_exists,
    })
}

/// Construct the canonical JSON payload persisted as an `ai_session` blob.
///
/// Functional scope: bundles a state-machine summary, a message-count summary,
/// the projected event stream, an inert compatibility `raw_hook_events` field,
/// and the in-memory session itself. The whole document is keyed by the
/// [`AI_SESSION_SCHEMA`] string so future schema migrations can detect old blobs.
fn build_ai_session_payload(session: &SessionState, provider: &dyn HookProvider) -> Value {
    let events = session
        .metadata
        .get(NORMALIZED_EVENTS_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Retain the v2 field shape for readers without allowing historical raw
    // hook payloads to enter a newly persisted object.
    let raw_events: Vec<Value> = Vec::new();
    let phase = session
        .metadata
        .get(SESSION_PHASE_METADATA_KEY)
        .and_then(Value::as_str)
        .unwrap_or("active");
    let provider_session_id = session
        .metadata
        .get(PROVIDER_SESSION_ID_METADATA_KEY)
        .and_then(Value::as_str)
        .unwrap_or(&session.id);
    // Provider-home locators are private, mutable capabilities. Canonical
    // ai_session history intentionally preserves the v2 field shape but never
    // serializes a path, including one left by a pre-ingress session cache.
    let transcript_path: Option<&str> = None;
    let last_assistant_message = session
        .metadata
        .get("last_assistant_message")
        .and_then(Value::as_str);

    json!({
        "schema": AI_SESSION_SCHEMA,
        "object_type": AI_SESSION_TYPE,
        "provider": provider.provider_name(),
        "ai_session_id": session.id,
        "provider_session_id": provider_session_id,
        "state_machine": {
            "phase": phase,
            "status": phase_status_label(phase),
            "event_count": events.len(),
            "tool_use_count": count_events(&events, "tool_use"),
            "compaction_count": count_events(&events, "compaction"),
            "started_at": first_event_timestamp(&events, "session_start"),
            "ended_at": first_event_timestamp(&events, "session_end"),
            "updated_at": session.updated_at.to_rfc3339(),
        },
        "summary": {
            "message_count": session.messages.len(),
            "user_message_count": session.messages.iter().filter(|message| message.role == "user").count(),
            "assistant_message_count": session.messages.iter().filter(|message| message.role == "assistant").count(),
            "last_assistant_message": last_assistant_message,
        },
        "transcript": {
            "path": transcript_path,
            "raw_event_count": raw_events.len(),
        },
        "events": events,
        "raw_hook_events": raw_events,
        "session": session,
        "ingest_meta": {
            "source": provider.source_name(),
            "provider": provider.provider_name(),
            "history_ref": history::ai_ref_name(),
            "ingested_at": Utc::now().to_rfc3339(),
        }
    })
}

/// Translate a phase string into a UI-friendly status label.
///
/// Boundary conditions: an unknown phase falls back to `"running"` so a
/// schema-drift session never produces an empty status.
fn phase_status_label(phase: &str) -> &'static str {
    match phase {
        "active" => "running",
        "stopped" => "idle",
        "ended" => "ended",
        _ => "running",
    }
}

/// Count normalized events with the given `kind`. Used to populate per-session
/// summary counters (tool uses, compactions, etc.).
fn count_events(events: &[Value], kind: &str) -> usize {
    events
        .iter()
        .filter(|value| value.get("kind").and_then(Value::as_str) == Some(kind))
        .count()
}

/// Return the timestamp of the first matching event, or `None` if no event has the
/// requested kind. Used to populate `started_at`/`ended_at` on the persisted
/// state-machine summary.
fn first_event_timestamp(events: &[Value], kind: &str) -> Option<String> {
    events
        .iter()
        .find(|value| value.get("kind").and_then(Value::as_str) == Some(kind))
        .and_then(|value| value.get("timestamp"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

impl SessionPhase {
    /// Stable string form persisted in `session_phase` metadata.
    fn as_str(self) -> &'static str {
        match self {
            SessionPhase::Active => "active",
            SessionPhase::Stopped => "stopped",
            SessionPhase::Ended => "ended",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::internal::ai::hooks::providers::{codex_provider, gemini_provider};

    // Scenario: pushing many keys past the cap evicts the oldest, never exceeding
    // `MAX_PROCESSED_EVENT_KEYS`.
    #[test]
    fn processed_event_keys_capped() {
        let mut session = SessionState::new("/tmp");
        for index in 0..(MAX_PROCESSED_EVENT_KEYS + 50) {
            append_processed_event_key(&mut session, format!("k{index}"));
        }

        let len = session
            .metadata
            .get(PROCESSED_EVENT_KEYS)
            .and_then(Value::as_array)
            .map(std::vec::Vec::len)
            .unwrap_or(0);
        assert_eq!(len, MAX_PROCESSED_EVENT_KEYS);
    }

    // Scenario: a SessionStart event sets the session phase to "active".
    #[test]
    fn unified_phase_metadata_key_is_used() {
        let envelope = SessionHookEnvelope {
            hook_event_name: "SessionStart".to_string(),
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: None,
            extra: Map::new(),
        };
        let event = gemini_provider()
            .parse_hook_event("SessionStart", &envelope)
            .expect("parse should succeed");
        let mut session = SessionState::new("/tmp");

        apply_hook_event(&mut session, &envelope, &event, "gemini");

        assert_eq!(
            session.metadata.get(SESSION_PHASE_METADATA_KEY),
            Some(&json!("active"))
        );
    }

    /// The public legacy AI-intent compatibility target cannot retain raw
    /// provider extras or transcript pointers. Its projection is redacted and
    /// keeps only the canonical event fields needed by the old session view.
    #[test]
    fn legacy_ai_intent_projection_drops_raw_envelope_and_redacts_fields() {
        let secret = format!("ghp_{}", "a".repeat(36));
        let raw_extra_marker = "LIBRA_TEST_RAW_ENVELOPE_MARKER_3a6d4e";
        let mut tool_input = Map::new();
        tool_input.insert(format!("credential_{secret}"), json!("safe value"));
        let envelope = SessionHookEnvelope {
            hook_event_name: "PostToolUse".to_string(),
            session_id: "codex-session".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: Some("/tmp/rollout.jsonl".to_string()),
            extra: serde_json::json!({
                "turn_id": "turn-1",
                "tool_use_id": "tool-1",
                "tool_name": "Bash",
                "tool_input": Value::Object(tool_input),
                "tool_response": {"stdout": secret.clone(), "exit_code": 0},
                "last_assistant_message": format!("finished with {secret}"),
                "permission_mode": "default",
                "untrusted_raw_marker": raw_extra_marker
            })
            .as_object()
            .expect("object payload")
            .clone(),
        };
        let event = codex_provider()
            .parse_hook_event("PostToolUse", &envelope)
            .expect("parse Codex hook");
        let (envelope, event) = prepare_ai_intent_persistence(envelope, event)
            .expect("sanitize legacy AI-intent projection");
        let mut session = SessionState::new("/tmp");
        session.metadata.insert(
            "raw_hook_events".to_string(),
            json!([{ "legacy": raw_extra_marker }]),
        );
        let legacy_transcript_marker = "/private/provider-home/legacy-transcript-marker.jsonl";
        session.metadata.insert(
            "transcript_path".to_string(),
            json!(legacy_transcript_marker),
        );
        sanitize_legacy_ai_intent_session(&mut session)
            .expect("sanitize historical legacy session state");

        apply_hook_event(&mut session, &envelope, &event, "codex");

        let normalized = session
            .metadata
            .get(NORMALIZED_EVENTS_KEY)
            .and_then(Value::as_array)
            .and_then(|events| events.last())
            .expect("normalized event");
        assert_eq!(normalized["provider_session_id"], json!("codex-session"));
        assert!(envelope.extra.is_empty());
        assert!(envelope.transcript_path.is_none());
        assert!(event.session_ref.is_none());
        assert_eq!(normalized["turn_id"], Value::Null);
        assert_eq!(normalized["tool_use_id"], Value::Null);
        assert_eq!(normalized["permission_mode"], Value::Null);
        assert_eq!(normalized["tool_input"], Value::Null);
        assert_eq!(normalized["tool_response"], Value::Null);
        assert!(!session.metadata.contains_key("raw_hook_events"));
        assert!(!session.metadata.contains_key("transcript_path"));

        let persisted =
            serde_json::to_string(&build_ai_session_payload(&session, codex_provider()))
                .expect("serialize sanitized legacy session payload");
        assert!(!persisted.contains(&secret));
        assert!(!persisted.contains(raw_extra_marker));
        assert!(!persisted.contains(legacy_transcript_marker));
        assert!(persisted.contains("<REDACTED:github-token>"));
    }

    // Scenario: a synthetic ended session includes the schema id, state machine
    // counters, message-count summary, and an intentionally null transcript
    // path because provider-home locators never enter immutable history.
    #[test]
    #[serial_test::serial(cwd, env)]
    fn v2_payload_contains_state_machine_and_summary() {
        let mut session = SessionState::new("/tmp/repo");
        session.id = "gemini__s-1".to_string();
        session.metadata.insert(
            PROVIDER_SESSION_ID_METADATA_KEY.to_string(),
            json!("s-1".to_string()),
        );
        session
            .metadata
            .insert(SESSION_PHASE_METADATA_KEY.to_string(), json!("ended"));
        session.metadata.insert(
            NORMALIZED_EVENTS_KEY.to_string(),
            json!([
                {"kind":"session_start","timestamp":"2026-01-01T00:00:00Z"},
                {"kind":"turn_start","timestamp":"2026-01-01T00:00:01Z"},
                {"kind":"tool_use","timestamp":"2026-01-01T00:00:02Z"},
                {"kind":"session_end","timestamp":"2026-01-01T00:00:03Z"}
            ]),
        );
        session
            .metadata
            .insert("transcript_path".to_string(), json!("/tmp/t.jsonl"));
        session
            .metadata
            .insert("last_assistant_message".to_string(), json!("done"));
        session.add_user_message("hello");
        session.add_assistant_message("done");

        let payload = build_ai_session_payload(&session, gemini_provider());

        assert_eq!(payload["schema"], json!(AI_SESSION_SCHEMA));
        assert_eq!(payload["provider"], json!("gemini"));
        assert_eq!(payload["object_type"], json!(AI_SESSION_TYPE));
        assert_eq!(payload["state_machine"]["phase"], json!("ended"));
        assert_eq!(payload["state_machine"]["tool_use_count"], json!(1));
        assert_eq!(payload["summary"]["message_count"], json!(2));
        assert_eq!(payload["summary"]["user_message_count"], json!(1));
        assert_eq!(payload["transcript"]["path"], Value::Null);
    }
}
