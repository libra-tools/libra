//! Shared runtime for provider lifecycle hook ingestion.
//!
//! When an external provider invokes `libra hooks <command>`, control lands in
//! [`process_hook_event_from_stdin`]. This function:
//! 1. Reads, size-bounds, and JSON-parses the stdin envelope.
//! 2. Validates it against the canonical schema.
//! 3. Asks the provider adapter to lower it into a [`LifecycleEvent`].
//! 4. Loads (or recovers) the persistent [`SessionState`], deduplicates the event,
//!    applies it, and on `SessionEnd` writes a content-addressed `ai_session` blob
//!    plus a history reference so other tools can read the session later.
//!
//! All bounded constants below (`MAX_*`) protect the runtime from runaway providers
//! that emit pathologically large or repetitive payloads.

use std::{io::Read, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use git_internal::hash::{HashKind, set_hash_kind};
use serde_json::{Value, json};

use super::{
    lifecycle::{
        LifecycleEvent, LifecycleEventKind, SessionHookEnvelope, append_raw_hook_event,
        apply_lifecycle_event, make_dedup_key, normalize_json_value,
        validate_session_hook_envelope,
    },
    provider::HookProvider,
};
use crate::{
    internal::{
        ai::{
            automation::dispatch_repo_hook_lifecycle_event_to_history,
            history::{AI_REF, HistoryManager},
            session::{SessionState, SessionStore},
        },
        config::ConfigKv,
        db,
    },
    utils::{client_storage::ClientStorage, error::emit_warning, object::write_git_object, util},
};

// Metadata keys persisted on `SessionState`. Centralised here so that ingestion,
// projection, and tests all see the same names.
const PROCESSED_EVENT_KEYS: &str = "processed_event_keys";
const NORMALIZED_EVENTS_KEY: &str = "normalized_events";
const PROVIDER_METADATA_KEY: &str = "provider";
const PROVIDER_SESSION_ID_METADATA_KEY: &str = "provider_session_id";
const SESSION_PHASE_METADATA_KEY: &str = "session_phase";
/// Separator inserted between provider name and the provider's native session ID
/// when forming Libra's namespaced AI session ID.
const SESSION_ID_DELIMITER: &str = "__";

// Resource bounds. The values are deliberately small enough to stay in memory for
// the longest plausible session while large enough to capture the events the agent
// actually needs for projection.
const MAX_STDIN_BYTES: usize = 1_048_576;
const MAX_PROCESSED_EVENT_KEYS: usize = 200;
const MAX_NORMALIZED_EVENTS: usize = 400;
const MAX_RAW_HOOK_EVENTS: usize = 200;
const MAX_TOOL_EVENTS: usize = 200;
const MAX_TRANSCRIPT_PATH_BYTES: usize = 4096;

/// Object type tag stamped on persisted AI session blobs.
pub const AI_SESSION_TYPE: &str = "ai_session";
/// Schema version. Bump when the persisted shape changes incompatibly.
pub const AI_SESSION_SCHEMA: &str = "libra.ai_session.v2";

/// Where a parsed hook event should land.
///
/// CEX-EntireIO Phase 1.5 introduces this enum so the same parsing /
/// validation pipeline can fan out to two refs:
///
/// - [`HookTarget::AiIntent`] — the canonical `refs/libra/intent` writer used
///   by `libra code` and the existing Claude/Gemini hook configs.
/// - [`HookTarget::AgentTraces`] — the external-Agent capture writer that
///   lives on `refs/libra/traces`. Fully wired: the runtime ingests the
///   lifecycle event into `agent_session` and writes an E4-libra checkpoint
///   commit on `SessionEnd` (see [`ingest_agent_traces_payload`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTarget {
    AiIntent,
    AgentTraces,
}

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

/// Combine a provider name with the provider's native session ID into Libra's
/// canonical ID.
///
/// Functional scope: the resulting string is used as a directory name and as a
/// metadata key, so it must round-trip without escaping. Both inputs are assumed
/// to come from validated envelopes (see [`validate_session_hook_envelope`]).
pub fn build_ai_session_id(provider: &str, provider_session_id: &str) -> String {
    format!("{provider}{SESSION_ID_DELIMITER}{provider_session_id}")
}

/// Strip session IDs down to a non-secret prefix for log output.
///
/// Functional scope: keeps the first eight characters and replaces the rest with
/// `***`. For very short IDs the entire value is masked. Used in `tracing` and
/// `eprintln!` calls to avoid leaking provider session identifiers into logs.
fn redact_session_id(session_id: &str) -> String {
    let mut chars = session_id.chars();
    let prefix: String = chars.by_ref().take(8).collect();
    if chars.next().is_some() {
        format!("{prefix}***")
    } else {
        "***".to_string()
    }
}

/// Top-level entry for `libra hooks <command>`.
///
/// Functional scope:
/// - Reads up to `MAX_STDIN_BYTES + 1` bytes from stdin and rejects oversize
///   payloads early.
/// - Parses the canonical [`SessionHookEnvelope`] and validates it.
/// - Asks `provider` to lower the envelope into a [`LifecycleEvent`] and confirms
///   the result matches the expected `expected_kind`.
/// - Loads the persistent session (creating a fresh one if missing, recovering
///   from corruption by archiving the bad cache file and starting clean).
/// - Updates session metadata, applies the lifecycle event, records dedup keys,
///   and on `SessionEnd` writes the final blob to the AI history ref.
///
/// Boundary conditions:
/// - Out-of-order delivery (e.g. the very first observed event is `ToolUse`)
///   creates a synthetic session marked with `recovered_from_out_of_order`.
/// - Corrupt session caches are archived for forensic inspection rather than
///   discarded silently — operators can still retrieve the original bytes from
///   the path in `corrupt_session_backup`.
/// - Errors during final persistence are surfaced; the partially-mutated session
///   is still saved so retries can converge.
///
/// See: `tests::v2_payload_contains_state_machine_and_summary`,
/// `tests::dedup_keys_remain_stable_across_providers`.
pub async fn process_hook_event_from_stdin(
    command: super::provider::ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
) -> Result<()> {
    process_hook_event_with_target(command, expected_kind, provider, HookTarget::AiIntent).await
}

/// Marker error for hook-envelope validation failures (size / UTF-8 / empty
/// / JSON / schema / transcript-path). A0-03: the command layer
/// (`command/agent/hooks.rs`) maps any ingest error whose chain carries this
/// to the stable `LBR-AGENT-008` (`AgentHookEnvelopeInvalid`) code; genuine
/// runtime failures (DB open, storage resolution, redaction) stay generic
/// fatals so an envelope reject never masquerades as an internal error.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HookEnvelopeInvalid(pub String);

/// Parametric form of [`process_hook_event_from_stdin`] that selects the
/// writer destination via [`HookTarget`].
///
/// For [`HookTarget::AiIntent`] the function is exactly the historical
/// behaviour (1:1 byte-compatible). For [`HookTarget::AgentTraces`] the
/// function runs the external-Agent capture ingest — stdin parse, validate,
/// redact, upsert into `agent_session`, and (on `SessionEnd`) write an
/// E4-libra checkpoint commit on `refs/libra/traces`.
pub async fn process_hook_event_with_target(
    command: super::provider::ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    target: HookTarget,
) -> Result<()> {
    if matches!(target, HookTarget::AgentTraces) {
        return ingest_agent_traces(command, expected_kind, provider).await;
    }

    let mut stdin_bytes = Vec::new();
    std::io::stdin()
        .take((MAX_STDIN_BYTES + 1) as u64)
        .read_to_end(&mut stdin_bytes)
        .context("failed to read stdin")?;
    if stdin_bytes.len() > MAX_STDIN_BYTES {
        bail!("hook input exceeds {MAX_STDIN_BYTES} bytes");
    }
    let stdin = String::from_utf8(stdin_bytes).context("hook input is not valid UTF-8")?;

    if stdin.trim().is_empty() {
        bail!("hook input is empty");
    }

    let envelope: SessionHookEnvelope =
        serde_json::from_str(&stdin).map_err(|err| anyhow!("invalid hook JSON payload: {err}"))?;
    validate_session_hook_envelope(&envelope, MAX_TRANSCRIPT_PATH_BYTES)?;

    let event = provider.parse_hook_event(&envelope.hook_event_name, &envelope)?;
    if event.kind != expected_kind {
        bail!(
            "hook event kind mismatch: expected '{}', got '{}' from hook_event_name '{}'",
            expected_kind,
            event.kind,
            envelope.hook_event_name
        );
    }

    let process_cwd = std::env::current_dir().context("failed to read current directory")?;
    let storage_path = util::try_get_storage_path(Some(process_cwd.clone()))
        .context("failed to resolve libra storage path from current directory")?;
    set_hash_kind_from_repo()
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
    if let Err(err) = session_store.adopt_legacy_subdir_session_if_needed(&ai_session_id) {
        tracing::warn!(
            session_id = %redact_session_id(&ai_session_id),
            error = %err,
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
            let archived_path = match session_store.archive_corrupt_session(&ai_session_id) {
                Ok(path) => path,
                Err(archive_err) => {
                    eprintln!(
                        "warning: failed to archive malformed session '{}': {}",
                        redact_session_id(&ai_session_id),
                        archive_err
                    );
                    None
                }
            };
            eprintln!(
                "warning: malformed session cache detected for '{}', recovering with a new in-memory session",
                redact_session_id(&ai_session_id)
            );

            let mut recovered = SessionState::new(&process_cwd_str);
            recovered.id = ai_session_id.clone();
            recovered.working_dir = process_cwd_str.clone();
            recovered
                .metadata
                .insert("recovered_from_corrupt_session".to_string(), json!(true));
            recovered
                .metadata
                .insert("recovery_error".to_string(), json!(err.to_string()));
            if let Some(path) = archived_path {
                recovered.metadata.insert(
                    "corrupt_session_backup".to_string(),
                    json!(path.to_string_lossy().to_string()),
                );
            }
            recovered
        }
        Err(err) => return Err(anyhow!("failed to load session: {err}")),
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
        session
            .metadata
            .insert("hook_reported_cwd".to_string(), json!(envelope.cwd.clone()));
        session
            .metadata
            .insert("hook_cwd_mismatch".to_string(), json!(true));
    } else {
        session.metadata.remove("hook_cwd_mismatch");
        session.metadata.remove("hook_reported_cwd");
    }

    let dedup_key = make_dedup_key(
        provider.dedup_identity_keys(),
        provider.lifecycle_fallback_events(),
        &envelope,
    );
    if dedup_hit(&session, dedup_key.as_deref()) {
        if event.kind != LifecycleEventKind::SessionEnd {
            return Ok(());
        }
        if session_persisted(&session) {
            return Ok(());
        }
    }

    apply_hook_event(&mut session, &envelope, &event, provider.provider_name());
    provider
        .post_process_event(command, &storage_path, &mut session, &envelope, &event)
        .context("provider hook post-processing failed")?;
    if let Some(event_key) = dedup_key {
        append_processed_event_key(&mut session, event_key);
    }

    if let Err(err) =
        dispatch_repo_hook_lifecycle_event_to_history(&process_cwd, &storage_path, event.kind).await
    {
        emit_warning(format!("failed to dispatch automation hook event: {err}"));
    }

    if event.kind == LifecycleEventKind::SessionEnd {
        match persist_session_history(&storage_path, &session, provider).await {
            Ok(outcome) => {
                session
                    .metadata
                    .insert("persisted".to_string(), json!(true));
                session
                    .metadata
                    .insert("persisted_at".to_string(), json!(Utc::now().to_rfc3339()));
                session
                    .metadata
                    .insert("history_ref".to_string(), json!(AI_REF));
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
                    Err(err) => {
                        session
                            .metadata
                            .insert("cleanup_failed".to_string(), json!(true));
                        session
                            .metadata
                            .insert("last_error".to_string(), json!(err.to_string()));
                    }
                }
            }
            Err(err) => {
                session
                    .metadata
                    .insert("persist_failed".to_string(), json!(true));
                session
                    .metadata
                    .insert("last_error".to_string(), json!(err.to_string()));
                session
                    .metadata
                    .insert("persisted".to_string(), json!(false));
                emit_warning(format!("failed to persist session history: {err}"));
                session_store.save(&session).map_err(|save_err| {
                    anyhow!("failed to save session after persistence failure: {save_err}")
                })?;
                return Err(err.context("session history persistence failed"));
            }
        }
    }

    session_store
        .save(&session)
        .map_err(|err| anyhow!("failed to save session: {err}"))?;
    Ok(())
}

/// External-Agent capture ingest (`refs/libra/traces`).
///
/// Reads the hook envelope from stdin, validates, parses to a
/// [`LifecycleEvent`], redacts free-form fields, and upserts into
/// `agent_session`. On `SessionEnd` it also writes an E4-libra checkpoint
/// commit on `refs/libra/traces` (it resolves the storage path and calls
/// [`ingest_agent_traces_payload`] with `Some(repo_path)`).
///
/// Boundary conditions:
/// - Idempotent on repeated `SessionStart` for the same provider session
///   (UPSERT keyed by `(agent_kind, provider_session_id)`).
/// - Best-effort on `agent_session` table absence: if the migration hasn't
///   run, returns a clear error so misconfigured installs surface a
///   diagnostic rather than panic.
async fn ingest_agent_traces(
    command: super::provider::ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
) -> Result<()> {
    let mut stdin_bytes = Vec::new();
    std::io::stdin()
        .take((MAX_STDIN_BYTES + 1) as u64)
        .read_to_end(&mut stdin_bytes)
        .context("failed to read stdin")?;
    if stdin_bytes.len() > MAX_STDIN_BYTES {
        // A0-03: envelope-size reject → LBR-AGENT-008 at the command layer.
        return Err(
            HookEnvelopeInvalid(format!("hook input exceeds {MAX_STDIN_BYTES} bytes")).into(),
        );
    }

    // Resolve storage and pin hash kind, exactly as the AiIntent flow does,
    // so this entry point's surface stays compatible with the rest of the
    // runtime.
    let process_cwd = std::env::current_dir().context("failed to read current directory")?;
    let storage_path = util::try_get_storage_path(Some(process_cwd.clone()))
        .context("failed to resolve libra storage path from current directory")?;
    set_hash_kind_from_repo()
        .await
        .context("failed to configure hash kind from repo config")?;

    let conn = db::get_db_conn_instance_for_path(&storage_path.join(util::DATABASE))
        .await
        .map_err(|err| anyhow!("failed to open libra database: {err}"))?;

    ingest_agent_traces_payload(
        &stdin_bytes,
        command,
        expected_kind,
        provider,
        &conn,
        Some(&storage_path),
    )
    .await
}

/// Connection-bound core of [`ingest_agent_traces`]. `pub` for the AG-19
/// span integration tests (`tests/agent_hook_span_test.rs`), which must
/// drive an in-process ingest under a fake tracing sink — NOT a stable
/// API, and intentionally not re-exported from the crate root. Unit tests
/// in this module use it for the same reason: no stdin stubbing, no
/// process-wide working-directory mutation. Fully deterministic given
/// `payload`, the connection, and (optionally) `repo_path` — round-2
/// BLOCK #10 acceptance criterion.
///
/// `repo_path` is the `.libra` directory used to resolve the Git object
/// store for checkpoint commit creation (Phase 2.1). Passing `None` skips
/// the checkpoint commit step on `SessionEnd` and only persists the
/// `agent_session` summary; tests use that path so they don't need a live
/// `libra init` workspace.
pub async fn ingest_agent_traces_payload(
    payload: &[u8],
    command: super::provider::ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    conn: &sea_orm::DatabaseConnection,
    repo_path: Option<&std::path::Path>,
) -> Result<()> {
    use sea_orm::{ConnectionTrait, Statement};

    use crate::internal::ai::observed_agents::{RedactionMatch, Redactor};

    // AG-19 observability (`agent.md` 落地执行补充规格 §6): one span per
    // ingest with required fields present and raw stdin / tool_input
    // deliberately absent. Fields unknown at open time are recorded later.
    let ingest_span = tracing::info_span!(
        "agent.hook.ingest",
        provider = provider.provider_name(),
        verb = %command,
        event_kind = tracing::field::Empty,
        frame_bytes = payload.len() as u64,
        validated = tracing::field::Empty,
        partial = tracing::field::Empty,
    );

    // A0-03: every envelope-content validation failure carries
    // [`HookEnvelopeInvalid`] so the command layer maps it to
    // `LBR-AGENT-008`, distinct from downstream store/runtime failures.
    if payload.len() > MAX_STDIN_BYTES {
        ingest_span.record("validated", false);
        return Err(
            HookEnvelopeInvalid(format!("hook input exceeds {MAX_STDIN_BYTES} bytes")).into(),
        );
    }
    let stdin = std::str::from_utf8(payload)
        .map_err(|err| HookEnvelopeInvalid(format!("hook input is not valid UTF-8: {err}")))?;
    if stdin.trim().is_empty() {
        ingest_span.record("validated", false);
        return Err(HookEnvelopeInvalid("hook input is empty".to_string()).into());
    }

    let envelope: SessionHookEnvelope = serde_json::from_str(stdin)
        .map_err(|err| HookEnvelopeInvalid(format!("invalid hook JSON payload: {err}")))?;
    if let Err(err) = validate_session_hook_envelope(&envelope, MAX_TRANSCRIPT_PATH_BYTES) {
        ingest_span.record("validated", false);
        return Err(HookEnvelopeInvalid(format!("{err}")).into());
    }
    ingest_span.record("validated", true);

    // Test-only crash knob (强制补强项 #10, `tests/agent_hook_crash_test.rs`):
    // panic after the payload has been fully read and validated but before
    // any database write, so crash-regression tests can prove a mid-ingest
    // failure leaves no partial session/checkpoint state and echoes no raw
    // stdin bytes. Mirrors the `LIBRA_TEST_*` env convention; never set in
    // production.
    if std::env::var_os("LIBRA_TEST_HOOK_PANIC_AFTER_READ").is_some() {
        panic!("test-injected hook panic (LIBRA_TEST_HOOK_PANIC_AFTER_READ)");
    }

    // AG-19 forward compatibility: an event name this build does not know
    // (newer upstream agent) is skipped-and-logged — never a panic, never
    // a checkpoint write, never a blocker for later known events.
    if !provider.recognizes_event(&envelope.hook_event_name) {
        ingest_span.record("partial", true);
        let _entered = ingest_span.enter();
        tracing::warn!(
            target: "agent.hook.ingest",
            provider = provider.provider_name(),
            hook_event_name = %envelope.hook_event_name,
            reason = "unknown_event_type",
            "skipping unrecognized lifecycle event name"
        );
        return Ok(());
    }

    let mut event = provider.parse_hook_event(&envelope.hook_event_name, &envelope)?;
    if event.kind != expected_kind {
        bail!(
            "hook event kind mismatch: expected '{}', got '{}' from hook_event_name '{}'",
            expected_kind,
            event.kind,
            envelope.hook_event_name
        );
    }
    ingest_span.record("event_kind", tracing::field::display(event.kind));
    ingest_span.record("partial", false);

    // Redact every free-form text field before it gets anywhere near
    // durable storage (AG-19 redaction-before-persist: prompt, tool
    // input/response and assistant message alike). We aggregate the
    // per-field reports into a single JSON document that lands in
    // `agent_session.redaction_report` so the persisted row is observably
    // scrubbed (Codex round-3 review: "assert observable redaction
    // outcome").
    let redaction_span = tracing::info_span!(
        "agent.redaction.apply",
        rules_hit = tracing::field::Empty,
        size_cap_triggered = false,
        fail_closed = false,
    );
    let (all_matches, bytes_scanned, bytes_redacted) = redaction_span.in_scope(|| {
        let redactor = Redactor::new_default();
        let mut all_matches: Vec<RedactionMatch> = Vec::new();
        let mut bytes_scanned: usize = 0;
        let mut bytes_redacted: usize = 0;
        let mut redact_string = |value: &mut Option<String>| {
            if let Some(text) = value.take() {
                let (redacted, report) = redactor.redact(text.as_bytes());
                *value = Some(String::from_utf8_lossy(redacted.bytes()).into_owned());
                bytes_scanned += report.bytes_scanned;
                bytes_redacted += report.bytes_redacted;
                all_matches.extend(report.matches);
            }
        };
        redact_string(&mut event.prompt);
        redact_string(&mut event.assistant_message);
        let mut redact_value = |value: &mut Option<serde_json::Value>| {
            if let Some(inner) = value.take() {
                let serialized = serde_json::to_vec(&inner).unwrap_or_default();
                let (redacted, report) = redactor.redact(&serialized);
                *value = serde_json::from_slice(redacted.bytes()).ok();
                bytes_scanned += report.bytes_scanned;
                bytes_redacted += report.bytes_redacted;
                all_matches.extend(report.matches);
            }
        };
        redact_value(&mut event.tool_input);
        redact_value(&mut event.tool_response);
        (all_matches, bytes_scanned, bytes_redacted)
    });
    redaction_span.record("rules_hit", all_matches.len() as u64);
    let redaction_report_json = serde_json::to_string(&serde_json::json!({
        "matches": all_matches,
        "bytes_scanned": bytes_scanned,
        "bytes_redacted": bytes_redacted,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    let backend = conn.get_database_backend();

    // If the migration has not run yet, fail loud rather than silently.
    let table_check = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1",
            [],
        ))
        .await
        .context("failed to query sqlite_master")?;
    if table_check.is_none() {
        bail!(
            "agent_session table does not exist; run `libra init` against this repository to apply migrations",
        );
    }

    let now = Utc::now().timestamp();
    let session_id = build_ai_session_id(provider.provider_name(), &envelope.session_id);
    let agent_kind = match provider.provider_name() {
        // Map HookProvider names to the closed set used by the
        // `agent_kind` CHECK constraint. Adding a new provider here
        // requires extending both this match and the migration.
        "claude" => "claude_code",
        "gemini" => "gemini",
        other => other,
    };

    // AG-19 owner filtering: first-writer-wins by recorded owner agent
    // kind per provider session id, so two adapters forwarding the same
    // underlying session cannot double-write checkpoints. SessionStart /
    // TurnStart are exempt (they may establish a claim); every other
    // event from a non-owner agent kind is skipped-and-logged, never a
    // hard error (`agent.md` AG-19 owner-filtering row).
    //
    // Ownership is `rowid ASC` — true insertion order. `agent_session` is
    // an ordinary rowid table and the UPSERT preserves rowids, so the
    // first physically-inserted claim wins permanently. The previous
    // `(started_at ASC, session_id ASC)` ordering used second-granularity
    // timestamps: a later-arriving row inserted within the same second
    // could win the lexicographic tiebreak AFTER the earlier row's owner
    // had already confirmed and written a checkpoint, yielding
    // checkpoints from two agent kinds for one provider session (caught
    // by `agent_lifecycle_event_test::
    // simultaneous_stop_race_yields_single_owner_checkpoints`).
    if !matches!(
        event.kind,
        LifecycleEventKind::SessionStart | LifecycleEventKind::TurnStart
    ) {
        let owner_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT agent_kind FROM agent_session WHERE provider_session_id = ? \
                 ORDER BY rowid ASC LIMIT 1",
                [envelope.session_id.clone().into()],
            ))
            .await
            .context("failed to query agent_session owner claim")?;
        if let Some(row) = owner_row {
            let owner_kind: String = row
                .try_get("", "agent_kind")
                .context("failed to read agent_session owner kind")?;
            if owner_kind != agent_kind {
                ingest_span.record("partial", true);
                let _entered = ingest_span.enter();
                tracing::warn!(
                    target: "agent.hook.ingest",
                    provider = provider.provider_name(),
                    owner = %owner_kind,
                    event_kind = %event.kind,
                    reason = "owner_mismatch",
                    "skipping non-owner lifecycle event (first-writer-wins)"
                );
                return Ok(());
            }
        }
    }

    let new_state = match event.kind {
        LifecycleEventKind::SessionStart => "active",
        LifecycleEventKind::SessionEnd => "stopped",
        LifecycleEventKind::Compaction => "condensed",
        LifecycleEventKind::CompactionCompleted => "active",
        _ => "active",
    };

    // UPSERT: insert a fresh row on first sight; otherwise just bump
    // `last_event_at`, `state`, and `redaction_report`. We key by
    // `(agent_kind, provider_session_id)` because the unique index already
    // lives there.
    //
    // Phase 4.1: also persist the `transcript_path` from the envelope
    // into `metadata_json` so `libra agent checkpoint rewind --apply`
    // can resolve the on-disk transcript file without re-running the
    // adapter's path-discovery heuristics.
    let concurrent_active =
        session_concurrent_active(conn, backend, event.kind, &session_id, &envelope.cwd).await?;
    let metadata_json = build_agent_session_metadata_json(&envelope, concurrent_active);
    let upsert_sql = "
        INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
            state = excluded.state,
            last_event_at = excluded.last_event_at,
            redaction_report = excluded.redaction_report,
            stopped_at = CASE WHEN excluded.state = 'stopped' THEN excluded.last_event_at
                              ELSE agent_session.stopped_at END,
            metadata_json = CASE
                WHEN length(excluded.metadata_json) > 2
                    THEN json_patch(agent_session.metadata_json, excluded.metadata_json)
                ELSE agent_session.metadata_json
            END
    ";
    let stopped_at: Option<i64> =
        matches!(event.kind, LifecycleEventKind::SessionEnd).then_some(now);
    conn.execute(Statement::from_sql_and_values(
        backend,
        upsert_sql,
        [
            session_id.clone().into(),
            agent_kind.into(),
            envelope.session_id.clone().into(),
            new_state.into(),
            envelope.cwd.clone().into(),
            metadata_json.into(),
            redaction_report_json.clone().into(),
            now.into(),
            now.into(),
            stopped_at.into(),
        ],
    ))
    .await
    .with_context(|| format!("failed to upsert agent_session for command '{command}'"))?;

    // entire.md §6.3 state machine: both `TurnEnd` (Stop — end of a turn,
    // session stays `active`) and `SessionEnd` (final) materialise a
    // `committed` checkpoint commit on `refs/libra/traces`, indexed in
    // `agent_checkpoint`. The checkpoint's tree carries metadata.json + the
    // redacted transcript blob (now the agent's full on-disk transcript, see
    // the writer); events-blob inclusion remains a follow-up. Per-turn
    // checkpoints give `libra agent checkpoint rewind` turn-level granularity.
    if matches!(
        event.kind,
        LifecycleEventKind::SessionEnd
            | LifecycleEventKind::TurnEnd
            | LifecycleEventKind::SubagentStart
            | LifecycleEventKind::SubagentEnd
    ) && let Some(repo) = repo_path
    {
        // AG-19 owner-race closure: the pre-upsert owner check above is a
        // fast path, but two providers racing on a fresh provider session
        // can BOTH pass it (each sees no owner) and both upsert. Re-read
        // the claim now that our row is durably in place. Ordering by
        // `rowid ASC` makes this confirmation *monotone*: an existing
        // row's rowid never changes and no later insert can obtain a
        // smaller one, so once a racer confirms itself as owner no
        // subsequently-arriving row can flip the answer — exactly one of
        // the racers writes the checkpoint. The loser keeps its metadata
        // row but is skipped fail-closed here.
        let confirmed_owner: String = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT agent_kind FROM agent_session WHERE provider_session_id = ? \
                 ORDER BY rowid ASC LIMIT 1",
                [envelope.session_id.clone().into()],
            ))
            .await
            .context("failed to re-confirm agent_session owner claim")?
            .map(|row| row.try_get("", "agent_kind"))
            .transpose()
            .context("failed to read confirmed agent_session owner kind")?
            .unwrap_or_else(|| agent_kind.to_string());
        if confirmed_owner != agent_kind {
            ingest_span.record("partial", true);
            let _entered = ingest_span.enter();
            tracing::warn!(
                target: "agent.hook.ingest",
                provider = provider.provider_name(),
                owner = %confirmed_owner,
                event_kind = %event.kind,
                reason = "owner_mismatch",
                "skipping checkpoint write for non-owner (post-upsert confirmation)"
            );
            return Ok(());
        }
        // A0-02: `SubagentStart` / `SubagentEnd` boundaries materialise an
        // independent `scope='subagent'` checkpoint (its own `traces`
        // commit + `agent_checkpoint` row) that carries parent session /
        // checkpoint linkage, so `checkpoint list/show/export/prune` and
        // `doctor` surface nested runs as first-class checkpoints instead of
        // leaving them as bounded `subagent_events` metadata on the main
        // checkpoint. `SessionEnd` / `TurnEnd` keep the `committed` path.
        if matches!(
            event.kind,
            LifecycleEventKind::SubagentStart | LifecycleEventKind::SubagentEnd
        ) {
            write_subagent_checkpoint(
                conn,
                repo,
                &session_id,
                &envelope,
                agent_kind,
                &event,
                &redaction_report_json,
                now,
            )
            .await?;
        } else {
            write_committed_checkpoint(
                conn,
                repo,
                &session_id,
                &envelope,
                agent_kind,
                &event,
                &redaction_report_json,
                &all_matches,
                now,
            )
            .await?;
        }
    }

    Ok(())
}

/// Build the JSON object stored in `agent_session.metadata_json`.
/// Currently captures the agent's on-disk transcript path so the rewind
/// path can locate the file without re-deriving provider conventions.
/// Returns `"{}"` when no useful fields are populated, so the upsert
/// CASE expression can detect the placeholder.
fn build_agent_session_metadata_json(
    envelope: &SessionHookEnvelope,
    concurrent_active: bool,
) -> String {
    let mut obj = serde_json::Map::new();
    if let Some(path) = envelope.transcript_path.as_deref()
        && !path.is_empty()
    {
        obj.insert(
            "transcript_path".to_string(),
            serde_json::Value::String(path.to_string()),
        );
    }
    // §6.3 state machine: a `TurnStart` that observes another `active`
    // session in the same `working_dir` records `concurrent_active=true`
    // (non-blocking). Only emit the field when set so the upsert's
    // placeholder detection (`length(metadata_json) > 2`) still treats an
    // otherwise-empty object as `"{}"`.
    if concurrent_active {
        obj.insert(
            "concurrent_active".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    if obj.is_empty() {
        return "{}".to_string();
    }
    serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string())
}

/// Detect whether a `TurnStart` is starting alongside another active agent
/// session in the same `working_dir`.
///
/// Per the traces state machine (`docs/development/commands/_general.md` §6.3),
/// a `TurnStart` (UserPromptSubmit) checks for other `active` sessions in
/// the same `working_dir`; finding any records `concurrent_active=true`
/// without blocking. Only `TurnStart` events newly raise the flag; the
/// marker's stickiness across the rest of the session is handled by the
/// metadata upsert, which merges (`json_patch`) rather than overwrites, so a
/// once-set `concurrent_active=true` survives later events that omit it.
async fn session_concurrent_active(
    conn: &sea_orm::DatabaseConnection,
    backend: sea_orm::DatabaseBackend,
    event_kind: LifecycleEventKind,
    libra_session_id: &str,
    working_dir: &str,
) -> Result<bool> {
    use sea_orm::{ConnectionTrait, Statement};

    // Only a fresh turn newly detects concurrency.
    if !matches!(event_kind, LifecycleEventKind::TurnStart) {
        return Ok(false);
    }

    // Count *other* active sessions sharing this working_dir.
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS peers FROM agent_session \
             WHERE state = 'active' AND working_dir = ? AND session_id <> ?",
            [working_dir.into(), libra_session_id.into()],
        ))
        .await
        .context("failed to count concurrent active sessions")?;
    let peers = row
        .and_then(|row| row.try_get_by::<i64, _>("peers").ok())
        .unwrap_or(0);
    Ok(peers > 0)
}

/// Write a `committed` checkpoint (for a `TurnEnd` or `SessionEnd` event):
/// materialise the E4-libra tree (metadata.json, manifest.json,
/// events/lifecycle.jsonl, transcript/<agent_kind>.jsonl,
/// redaction_report.json, content_hash.txt), append a commit on
/// `refs/libra/traces`, and insert the corresponding `agent_checkpoint`
/// row. Errors are surfaced verbatim — a failure here means the ingest cannot
/// acknowledge the checkpoint to the caller.
///
/// `event` is the (already-redacted) triggering lifecycle event; it feeds
/// both the canonical `events/lifecycle.jsonl` line and metadata.json's
/// `model` field, and its redacted prompt is the transcript fallback when
/// the adapter advertises no readable transcript.
///
/// Write sequence + crash windows (AG-20, see the write-sequence matrix in
/// `docs/development/tracing/agent.md`): an in-flight marker is (best-effort)
/// persisted BEFORE stage (a) and cleared AFTER stage (d); between ref CAS
/// (c) and catalog INSERT (d) the catalog is probed by `traces_commit` so a
/// retry — or a doctor repair that already backfilled the row — never
/// double-inserts.
#[allow(clippy::too_many_arguments)]
async fn write_committed_checkpoint(
    conn: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    libra_session_id: &str,
    envelope: &SessionHookEnvelope,
    agent_kind: &str,
    event: &LifecycleEvent,
    redaction_report_json: &str,
    redaction_matches: &[crate::internal::ai::observed_agents::RedactionMatch],
    now: i64,
) -> Result<()> {
    use crate::internal::ai::{
        history::{self, CheckpointCommitParams, CheckpointScope, HistoryManager},
        observed_agents::{AgentKind, AgentSessionCtx, RedactedBytes, Redactor, agent_for},
    };

    let redacted_prompt = event.prompt.as_deref();

    // Capture the agent's full on-disk transcript for the checkpoint blob.
    // The prompt-only stopgap is replaced by the adapter's `read_transcript`:
    // resolve the provider's ObservedAgent, read the raw transcript, and
    // redact it before it touches durable storage (entire.md §8.1 / §13 P0).
    // The transcript's redaction matches are merged into the checkpoint's
    // `redaction_report` so the stored report stays consistent with the
    // stored blob. Falls back to the already-redacted prompt when the
    // adapter is unknown, advertises no transcript (no path, or the file is
    // absent/empty), or errors — the SessionEnd checkpoint must still write.
    let mut report_value = serde_json::from_str::<serde_json::Value>(redaction_report_json)
        .unwrap_or_else(|_| serde_json::json!({}));
    let prompt_fallback =
        || RedactedBytes::new_unchecked(redacted_prompt.unwrap_or("").as_bytes().to_vec());
    // AG-21: keep the raw transcript bytes in scope (never persisted) so
    // the adapter's extraction capabilities can derive metadata from them
    // after the redacted blob is produced.
    let mut transcript_raw_for_extraction: Option<Vec<u8>> = None;
    let transcript_redacted = match AgentKind::from_db_str(agent_kind) {
        Some(kind) => {
            let adapter = agent_for(kind);
            let ctx = AgentSessionCtx {
                session_id: libra_session_id.to_string(),
                provider_session_id: envelope.session_id.clone(),
                working_dir: std::path::PathBuf::from(&envelope.cwd),
                transcript_path: envelope
                    .transcript_path
                    .as_ref()
                    .map(std::path::PathBuf::from),
            };
            // Security gate (entire.md §8.1 / §13 P0): `transcript_path` comes
            // from the untrusted hook envelope. Only read + persist it when it
            // resolves inside the provider's own home-relative transcript root
            // (e.g. `~/.claude`); a forged path pointing at an arbitrary file
            // must never be copied into the syncable traces blob.
            let trusted = ctx
                .transcript_path
                .as_deref()
                .is_some_and(|path| transcript_path_within_provider_root(adapter, path));
            if !trusted {
                prompt_fallback()
            } else {
                match adapter.read_transcript(&ctx) {
                    Ok(Some(raw)) if !raw.is_empty() => {
                        let (redacted, report) = Redactor::new_default().redact(&raw);
                        merge_redaction_report_into(&mut report_value, &report);
                        transcript_raw_for_extraction = Some(raw);
                        redacted
                    }
                    Ok(_) => prompt_fallback(),
                    Err(err) => {
                        tracing::warn!(
                            agent_kind,
                            error = %format!("{err:#}"),
                            "failed to read agent transcript for checkpoint; \
                             falling back to the redacted prompt"
                        );
                        prompt_fallback()
                    }
                }
            }
        }
        None => prompt_fallback(),
    };

    // Build metadata.json (external schema v2 — v1 fields plus `model`;
    // strictly additive so v1 readers keep working). `model` mirrors the
    // E4-entire tolerance: taken from the triggering lifecycle event when
    // present, else the literal "unknown".
    let redaction_report_value = report_value.clone();
    // AG-21 transcript intelligence: derive token usage / model / skill
    // events from the raw transcript via the adapter's capability
    // accessors. Strictly fail-open — extraction problems mark the
    // metadata `partial` with redacted warnings and never block the
    // checkpoint write (redaction/path/write paths stay fail-closed).
    let extraction_value =
        build_extraction_metadata(agent_kind, transcript_raw_for_extraction.as_deref());
    let metadata = serde_json::json!({
        "schema_version": history::CHECKPOINT_METADATA_SCHEMA_VERSION,
        "checkpoint_id": null, // filled in below once we have the UUID
        "session_id": libra_session_id,
        "agent_kind": agent_kind,
        "scope": "committed",
        "provider_session_id": envelope.session_id,
        "working_dir": envelope.cwd,
        "model": checkpoint_model_field(event.model.as_ref()),
        "redaction_report": report_value,
        "created_at": now,
        "extraction": extraction_value,
    });

    // Fresh id per write attempt — it names both the catalog row and the
    // tree path, so it must exist before any blob/tree is built. Retry
    // dedup happens later at the traces_commit probe, not here.
    let checkpoint_id = uuid::Uuid::new_v4().to_string();
    let mut metadata = metadata;
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "checkpoint_id".to_string(),
            serde_json::Value::String(checkpoint_id.clone()),
        );
    }
    let metadata_bytes =
        serde_json::to_vec_pretty(&metadata).context("serialize checkpoint metadata")?;
    // AG-19 / G4 redaction-before-persist: every blob entering the traces
    // writer is a `RedactedBytes`, so no `&[u8]` can reach the checkpoint
    // sink. The pass is idempotent defense-in-depth — these buffers are
    // already built from redacted extraction outputs / rule-hit statistics,
    // and the redactor skips existing `<REDACTED:…>` spans.
    let (metadata_redacted, _) = Redactor::new_default().redact(&metadata_bytes);

    // Canonical E3-JSONL evidence line(s) for events/lifecycle.jsonl —
    // exactly the redacted triggering event today; the slice API keeps
    // multi-event batches source-compatible.
    let canonical_ctx = super::lifecycle::CanonicalEventContext {
        agent_kind,
        session_id: libra_session_id,
        provider_session_id: &envelope.session_id,
        provenance: serde_json::json!({
            "channel": "hook",
            "hook_event_name": envelope.hook_event_name,
        }),
    };
    let lifecycle_events_jsonl =
        super::lifecycle::lifecycle_events_to_canonical_jsonl(&[event], &canonical_ctx);
    let (lifecycle_events_redacted, _) = Redactor::new_default().redact(&lifecycle_events_jsonl);
    let redaction_report_bytes = serde_json::to_vec_pretty(&redaction_report_value)
        .context("serialize checkpoint redaction_report.json")?;
    let (redaction_report_redacted, _) = Redactor::new_default().redact(&redaction_report_bytes);

    // AG-20 observability (`agent.md` §6): one span per checkpoint write.
    // Required fields: checkpoint_id, session_id, stage (progression),
    // cas_retries, object_count. The transcript body is deliberately never
    // recorded.
    let write_span = tracing::info_span!(
        "agent.checkpoint.write",
        checkpoint_id = %checkpoint_id,
        session_id = %libra_session_id,
        stage = tracing::field::Empty,
        cas_retries = tracing::field::Empty,
        object_count = tracing::field::Empty,
    );
    write_span.record("stage", "marker");

    // Window A/B guard: persist the in-flight marker BEFORE stage (a).
    // Best-effort — a marker failure warns and continues (the checkpoint
    // must not be lost over its advisory guard), but when the call
    // succeeds the marker IS durably in SQLite before any blob exists.
    let marker = history::TracesInflightMarker::new(
        libra_session_id,
        &checkpoint_id,
        Utc::now().timestamp_millis(),
    );
    if let Err(err) = history::write_traces_inflight_marker(conn, &marker).await {
        tracing::warn!(
            checkpoint_id = %checkpoint_id,
            error = %format!("{err:#}"),
            "failed to persist traces in-flight marker; continuing without window guard"
        );
    }

    let objects_dir = repo_path.join("objects");
    std::fs::create_dir_all(&objects_dir).context("create objects dir for checkpoint commit")?;
    let storage = std::sync::Arc::new(crate::utils::client_storage::ClientStorage::init(
        objects_dir,
    ));
    let manager = HistoryManager::new_with_ref(
        storage,
        repo_path.to_path_buf(),
        std::sync::Arc::new(conn.clone()),
        crate::internal::branch::TRACES_BRANCH,
    );

    // Resolve the user-branch HEAD via the typed helper so we can
    // distinguish "no HEAD yet (unborn)" from real storage errors. The
    // empty-string conflation produced by the lossy wrapper was flagged
    // in the Codex Phase-2 round-1 review.
    //
    // Three semantic cases land in `parent_commit`:
    // - `Some(hash)` — repo has at least one commit and HEAD resolves.
    // - `None` from typed `Ok(None)` — HEAD is born but the branch is
    //   commit-less (e.g. immediately after `libra init`).
    // - `None` from `BranchStoreError::Corrupt { "HEAD reference is missing" }`
    //   — the schema is wired but the HEAD row was never seeded. This
    //   shows up in test fixtures that bootstrap the migrations without
    //   running `initialize_refs`. Functionally equivalent to "unborn" for
    //   the traces writer, so we coerce to `None` rather than
    //   failing the whole ingest.
    let parent_commit: Option<String> =
        match crate::internal::head::Head::current_commit_result_with_conn(conn).await {
            Ok(commit) => commit.map(|h| h.to_string()),
            Err(crate::internal::branch::BranchStoreError::Corrupt { detail, .. })
                if detail.contains("HEAD reference is missing") =>
            {
                None
            }
            Err(err) => {
                return Err(anyhow!(
                    "failed to resolve HEAD while writing checkpoint: {err}"
                ));
            }
        };

    // Stages (a)–(c): blobs, trees, commit, ref CAS.
    write_span.record("stage", "append");
    let written = manager
        .append_checkpoint_commit(CheckpointCommitParams {
            checkpoint_id: &checkpoint_id,
            session_id: libra_session_id,
            agent_kind,
            parent_commit: parent_commit.as_deref(),
            scope: CheckpointScope::Committed,
            tool_use_id: None,
            metadata_json: &metadata_redacted,
            transcript_redacted: &transcript_redacted,
            lifecycle_events_jsonl: &lifecycle_events_redacted,
            redaction_report_json: &redaction_report_redacted,
        })
        .await
        .context("failed to append checkpoint commit on traces")?;
    write_span.record("cas_retries", written.cas_retries);
    write_span.record("object_count", written.object_count);
    write_span.record("stage", "ref_cas_done");

    // Extend the marker with the CAS'd commit + top OIDs (best-effort) so
    // a window-B prune can protect the exact commit until stage (d) lands.
    let mut committed_marker = marker.clone();
    committed_marker.commit = Some(written.commit_hash.to_string());
    committed_marker.oids = vec![
        written.tree_oid.to_string(),
        written.metadata_blob_oid.to_string(),
    ];
    if let Err(err) = history::write_traces_inflight_marker(conn, &committed_marker).await {
        tracing::warn!(
            checkpoint_id = %checkpoint_id,
            error = %format!("{err:#}"),
            "failed to refresh traces in-flight marker after ref CAS"
        );
    }

    // Stage (d), idempotent.
    write_span.record("stage", "catalog");
    let inserted = insert_agent_checkpoint_row_idempotent(
        conn,
        &AgentCheckpointRow {
            checkpoint_id: &checkpoint_id,
            session_id: libra_session_id,
            parent_commit: parent_commit.as_deref(),
            tree_oid: &written.tree_oid.to_string(),
            metadata_blob_oid: &written.metadata_blob_oid.to_string(),
            traces_commit: &written.commit_hash.to_string(),
            created_at: now,
        },
    )
    .await?;
    if !inserted {
        write_span.record("stage", "catalog_deduped");
    }

    // Stage (d) complete — release the window guard (best-effort; an
    // orphaned marker expires via its TTL).
    if let Err(err) =
        history::clear_traces_inflight_marker(conn, libra_session_id, &checkpoint_id).await
    {
        tracing::warn!(
            checkpoint_id = %checkpoint_id,
            error = %format!("{err:#}"),
            "failed to clear traces in-flight marker after catalog insert"
        );
    }
    write_span.record("stage", "done");

    // Suppress the unused-warning for redaction_matches; reserved for a
    // Phase 3 enhancement that adds per-rule counters to metadata.
    let _ = redaction_matches;
    Ok(())
}

/// A0-02: materialise an independent `scope='subagent'` checkpoint at a
/// `SubagentStart` / `SubagentEnd` boundary.
///
/// Unlike [`write_committed_checkpoint`], this path deliberately does NOT
/// re-read the parent agent's on-disk transcript — a subagent boundary event
/// carries no separate transcript, and the parent session's `committed`
/// checkpoints already capture the full transcript. The subagent checkpoint's
/// tree carries a compact `metadata.json` describing the boundary (kind,
/// tool, source, timestamp, parent linkage) with an empty transcript blob, so
/// it is fully `list/show/export/prune/doctor`-able while staying cheap to
/// write. Parent linkage (`parent_checkpoint_id`) points at the session's
/// most recent `committed` checkpoint so `checkpoint show` can walk from a
/// nested run back to the enclosing turn/session checkpoint.
///
/// Crash-safety mirrors the committed path: an in-flight marker is persisted
/// before the traces commit and cleared after the catalog INSERT, so a
/// concurrent prune in the ref-CAS→catalog window cannot drop the commit.
#[allow(clippy::too_many_arguments)]
async fn write_subagent_checkpoint(
    conn: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    libra_session_id: &str,
    envelope: &SessionHookEnvelope,
    agent_kind: &str,
    event: &LifecycleEvent,
    redaction_report_json: &str,
    now: i64,
) -> Result<()> {
    use crate::internal::ai::{
        history::{self, CheckpointCommitParams, CheckpointScope, HistoryManager},
        observed_agents::{RedactedBytes, Redactor},
    };

    // Resolve the parent `committed` checkpoint (if any) for linkage.
    let parent_checkpoint_id = latest_committed_checkpoint_id(conn, libra_session_id).await?;

    let boundary = match event.kind {
        LifecycleEventKind::SubagentStart => "start",
        _ => "end",
    };
    // The subagent boundary hook envelope carries only `session_id` + `cwd`
    // (see the Codex `SubagentStop` row in agent.md) — no distinct subagent
    // id and no tool-use id. We therefore leave `subagent_session_id` /
    // `tool_use_id` NULL rather than inventing them: in particular
    // `event.session_ref` is filled from the transcript PATH by
    // `build_lifecycle_event`, so persisting it as a subagent id would both
    // mislabel the column and leak a filesystem path. The tool NAME (if the
    // provider ever surfaces one) is recorded in the metadata / description
    // for context only; the id columns stay reserved for a future provider
    // that emits a real subagent/tool-use id.
    let tool_name = event.tool_name.clone();
    let subagent_session_id: Option<String> = None;
    let tool_use_id: Option<String> = None;
    // Redact the description before it lands in either the catalog row or
    // metadata.json — `tool_name` is event-derived and could carry a path.
    // Redacting once keeps the row's `description` column and the
    // metadata.json `description` (which doctor rebuilds the row from) byte
    // -identical.
    let description = redact_extracted_string(&match tool_name.as_deref() {
        Some(t) => format!("subagent {boundary} via {t}"),
        None => format!("subagent {boundary}"),
    });

    let checkpoint_id = uuid::Uuid::new_v4().to_string();

    // Compact metadata.json. Boundary/source/tool are event-derived and may
    // reference file paths, so the whole object passes the default redactor
    // before persist (same discipline as the committed writer's metadata).
    let source_redacted = event.source.clone().map(redact_extracted_json);
    let metadata = serde_json::json!({
        "schema_version": 1,
        "checkpoint_id": checkpoint_id,
        "session_id": libra_session_id,
        "agent_kind": agent_kind,
        "scope": "subagent",
        "provider_session_id": envelope.session_id,
        "working_dir": envelope.cwd,
        "created_at": now,
        // Flat linkage fields (A0-02): the subagent-scope class-2 doctor
        // repair rebuilds the catalog row straight from these, mirroring the
        // committed path's metadata-driven repair.
        "parent_checkpoint_id": parent_checkpoint_id,
        "subagent_session_id": subagent_session_id,
        "tool_use_id": tool_use_id,
        "description": description,
        "subagent": {
            "boundary": boundary,
            "kind": event.kind.to_string(),
            "tool": tool_name,
            "source": source_redacted,
            "timestamp": event.timestamp.to_rfc3339(),
        },
    });
    let metadata_bytes =
        serde_json::to_vec_pretty(&metadata).context("serialize subagent checkpoint metadata")?;
    let (metadata_redacted, _) = Redactor::new_default().redact(&metadata_bytes);

    // Empty transcript — the boundary carries no separate transcript body.
    let transcript_redacted = RedactedBytes::new_unchecked(Vec::new());

    // One canonical lifecycle evidence line for the boundary event.
    let canonical_ctx = super::lifecycle::CanonicalEventContext {
        agent_kind,
        session_id: libra_session_id,
        provider_session_id: &envelope.session_id,
        provenance: serde_json::json!({
            "channel": "hook",
            "hook_event_name": envelope.hook_event_name,
        }),
    };
    // Defense-in-depth (Codex A0-02 re-review): `build_lifecycle_event`
    // fills `session_ref` from the transcript PATH, and the default redactor
    // does not reliably scrub arbitrary absolute paths — so a `SubagentStop`
    // serialized verbatim would persist that local path in the syncable
    // `events/lifecycle.jsonl` blob. Clear `session_ref` (and the unused
    // prompt body) on a sanitized copy before serialization; a subagent
    // boundary marker needs neither field.
    let mut sidecar_event = event.clone();
    sidecar_event.session_ref = None;
    sidecar_event.prompt = None;
    let lifecycle_events_jsonl =
        super::lifecycle::lifecycle_events_to_canonical_jsonl(&[&sidecar_event], &canonical_ctx);
    let (lifecycle_events_redacted, _) = Redactor::new_default().redact(&lifecycle_events_jsonl);

    let report_value = serde_json::from_str::<serde_json::Value>(redaction_report_json)
        .unwrap_or_else(|_| serde_json::json!({}));
    let redaction_report_bytes = serde_json::to_vec_pretty(&report_value)
        .context("serialize subagent checkpoint redaction_report.json")?;
    let (redaction_report_redacted, _) = Redactor::new_default().redact(&redaction_report_bytes);

    // Parent user-branch HEAD (same semantics/tolerance as committed path).
    let parent_commit: Option<String> =
        match crate::internal::head::Head::current_commit_result_with_conn(conn).await {
            Ok(commit) => commit.map(|h| h.to_string()),
            Err(crate::internal::branch::BranchStoreError::Corrupt { detail, .. })
                if detail.contains("HEAD reference is missing") =>
            {
                None
            }
            Err(err) => {
                return Err(anyhow!(
                    "failed to resolve HEAD while writing subagent checkpoint: {err}"
                ));
            }
        };

    let marker = history::TracesInflightMarker::new(
        libra_session_id,
        &checkpoint_id,
        Utc::now().timestamp_millis(),
    );
    if let Err(err) = history::write_traces_inflight_marker(conn, &marker).await {
        tracing::warn!(
            checkpoint_id = %checkpoint_id,
            error = %format!("{err:#}"),
            "failed to persist subagent traces in-flight marker; continuing"
        );
    }

    let objects_dir = repo_path.join("objects");
    std::fs::create_dir_all(&objects_dir)
        .context("create objects dir for subagent checkpoint commit")?;
    let storage = std::sync::Arc::new(crate::utils::client_storage::ClientStorage::init(
        objects_dir,
    ));
    let manager = HistoryManager::new_with_ref(
        storage,
        repo_path.to_path_buf(),
        std::sync::Arc::new(conn.clone()),
        crate::internal::branch::TRACES_BRANCH,
    );

    let written = manager
        .append_checkpoint_commit(CheckpointCommitParams {
            checkpoint_id: &checkpoint_id,
            session_id: libra_session_id,
            agent_kind,
            parent_commit: parent_commit.as_deref(),
            scope: CheckpointScope::Subagent,
            tool_use_id: tool_use_id.as_deref(),
            metadata_json: &metadata_redacted,
            transcript_redacted: &transcript_redacted,
            lifecycle_events_jsonl: &lifecycle_events_redacted,
            redaction_report_json: &redaction_report_redacted,
        })
        .await
        .context("failed to append subagent checkpoint commit on traces")?;

    let mut committed_marker = marker.clone();
    committed_marker.commit = Some(written.commit_hash.to_string());
    committed_marker.oids = vec![
        written.tree_oid.to_string(),
        written.metadata_blob_oid.to_string(),
    ];
    if let Err(err) = history::write_traces_inflight_marker(conn, &committed_marker).await {
        tracing::warn!(
            checkpoint_id = %checkpoint_id,
            error = %format!("{err:#}"),
            "failed to refresh subagent traces in-flight marker after ref CAS"
        );
    }

    insert_subagent_checkpoint_row_idempotent(
        conn,
        &SubagentCheckpointRow {
            checkpoint_id: &checkpoint_id,
            session_id: libra_session_id,
            parent_commit: parent_commit.as_deref(),
            parent_checkpoint_id: parent_checkpoint_id.as_deref(),
            subagent_session_id: subagent_session_id.as_deref(),
            tool_use_id: tool_use_id.as_deref(),
            description: Some(&description),
            tree_oid: &written.tree_oid.to_string(),
            metadata_blob_oid: &written.metadata_blob_oid.to_string(),
            traces_commit: &written.commit_hash.to_string(),
            created_at: now,
        },
    )
    .await?;

    if let Err(err) =
        history::clear_traces_inflight_marker(conn, libra_session_id, &checkpoint_id).await
    {
        tracing::warn!(
            checkpoint_id = %checkpoint_id,
            error = %format!("{err:#}"),
            "failed to clear subagent traces in-flight marker after catalog insert"
        );
    }

    Ok(())
}

/// Most-recent `committed` checkpoint id for a session, used as the
/// `parent_checkpoint_id` linkage of a freshly-materialised subagent
/// checkpoint. Returns `None` when the session has no committed checkpoint
/// yet (the subagent ran before the first turn/session checkpoint landed).
async fn latest_committed_checkpoint_id(
    conn: &sea_orm::DatabaseConnection,
    session_id: &str,
) -> Result<Option<String>> {
    use sea_orm::{ConnectionTrait, Statement};

    let row = conn
        .query_one(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint \
             WHERE session_id = ? AND scope = 'committed' \
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [session_id.into()],
        ))
        .await
        .context("failed to resolve latest committed checkpoint for subagent linkage")?;
    row.map(|r| r.try_get::<String>("", "checkpoint_id"))
        .transpose()
        .context("failed to read parent committed checkpoint id")
}

/// Column values for one `scope='subagent'` `agent_checkpoint` row (A0-02).
/// Carries the subagent-specific linkage columns the committed path leaves
/// NULL: `parent_checkpoint_id`, `subagent_session_id`, `tool_use_id`,
/// `description`.
#[derive(Debug)]
pub struct SubagentCheckpointRow<'a> {
    pub checkpoint_id: &'a str,
    pub session_id: &'a str,
    pub parent_commit: Option<&'a str>,
    pub parent_checkpoint_id: Option<&'a str>,
    pub subagent_session_id: Option<&'a str>,
    pub tool_use_id: Option<&'a str>,
    pub description: Option<&'a str>,
    pub tree_oid: &'a str,
    pub metadata_blob_oid: &'a str,
    /// `CheckpointCommit.commit_hash` — lands in the `traces_commit` column.
    pub traces_commit: &'a str,
    pub created_at: i64,
}

/// Idempotent INSERT of a `scope='subagent'` catalog row, mirroring
/// [`insert_agent_checkpoint_row_idempotent`]: probe by `traces_commit`
/// first (a crash-retry or doctor backfill must not double-insert), then
/// INSERT with an `ON CONFLICT(checkpoint_id) DO NOTHING` backstop. Returns
/// `true` when this call inserted the row.
pub async fn insert_subagent_checkpoint_row_idempotent(
    conn: &sea_orm::DatabaseConnection,
    row: &SubagentCheckpointRow<'_>,
) -> Result<bool> {
    use sea_orm::{ConnectionTrait, Statement};

    use crate::internal::ai::history;

    if let Some(existing_id) =
        history::agent_checkpoint_id_for_traces_commit(conn, row.traces_commit).await?
    {
        tracing::info!(
            checkpoint_id = %row.checkpoint_id,
            existing_checkpoint_id = %existing_id,
            "agent_checkpoint subagent row already present for traces commit; skipping INSERT"
        );
        return Ok(false);
    }
    let parent_commit_value: sea_orm::Value = row.parent_commit.map(str::to_string).into();
    let parent_checkpoint_value: sea_orm::Value =
        row.parent_checkpoint_id.map(str::to_string).into();
    let subagent_session_value: sea_orm::Value = row.subagent_session_id.map(str::to_string).into();
    let tool_use_value: sea_orm::Value = row.tool_use_id.map(str::to_string).into();
    let description_value: sea_orm::Value = row.description.map(str::to_string).into();
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at
             ) VALUES (?, ?, ?, 'subagent', ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                row.checkpoint_id.into(),
                row.session_id.into(),
                parent_checkpoint_value,
                parent_commit_value,
                row.tree_oid.into(),
                row.metadata_blob_oid.into(),
                row.traces_commit.into(),
                tool_use_value,
                subagent_session_value,
                description_value,
                row.created_at.into(),
            ],
        ))
        .await
        .context("failed to insert subagent agent_checkpoint row")?;
    Ok(result.rows_affected() > 0)
}

/// Column values for one `agent_checkpoint` row (scope is always
/// `committed` on this path).
#[derive(Debug)]
pub struct AgentCheckpointRow<'a> {
    pub checkpoint_id: &'a str,
    pub session_id: &'a str,
    pub parent_commit: Option<&'a str>,
    pub tree_oid: &'a str,
    pub metadata_blob_oid: &'a str,
    /// `CheckpointCommit.commit_hash` — lands in the `traces_commit` column.
    pub traces_commit: &'a str,
    pub created_at: i64,
}

/// Stage (d) of the checkpoint write sequence, made idempotent (AG-20):
/// probe the catalog by `traces_commit` first — a crash-retry of the
/// ingest, or a doctor repair that already backfilled the row from the
/// ref, must not insert a second row for the same commit — then INSERT
/// with an `ON CONFLICT(checkpoint_id) DO NOTHING` backstop covering a
/// racer inserting the same checkpoint id between probe and INSERT.
///
/// Returns `true` when this call inserted the row, `false` when an
/// existing row (either match) made it a no-op. `pub` for the AG-20
/// crash-retry integration tests (`tests/agent_checkpoint_export_test.rs`)
/// — NOT a stable API.
pub async fn insert_agent_checkpoint_row_idempotent(
    conn: &sea_orm::DatabaseConnection,
    row: &AgentCheckpointRow<'_>,
) -> Result<bool> {
    use sea_orm::{ConnectionTrait, Statement};

    use crate::internal::ai::history;

    if let Some(existing_id) =
        history::agent_checkpoint_id_for_traces_commit(conn, row.traces_commit).await?
    {
        tracing::info!(
            checkpoint_id = %row.checkpoint_id,
            existing_checkpoint_id = %existing_id,
            "agent_checkpoint row already present for traces commit; skipping INSERT"
        );
        return Ok(false);
    }
    let parent_commit_value: sea_orm::Value = row.parent_commit.map(str::to_string).into();
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES (?, ?, 'committed', ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                row.checkpoint_id.into(),
                row.session_id.into(),
                parent_commit_value,
                row.tree_oid.into(),
                row.metadata_blob_oid.into(),
                row.traces_commit.into(),
                row.created_at.into(),
            ],
        ))
        .await
        .context("failed to insert agent_checkpoint row")?;
    Ok(result.rows_affected() > 0)
}

/// Extract metadata.json's `model` field from a lifecycle event's `model`
/// value, mirroring the E4-entire missing-`model` tolerance: absent or
/// unrecognisable shapes become the literal `"unknown"` rather than an
/// error. Providers emit either a plain string or an object carrying an
/// id/name-ish key.
/// Redact a single extracted string (model id, file path) through the
/// default `Redactor` before it lands in checkpoint metadata. Extraction
/// fields are derived from the raw (un-redacted) transcript, so they must
/// pass the same scrubbing the transcript blob does.
fn redact_extracted_string(value: &str) -> String {
    use crate::internal::ai::observed_agents::Redactor;
    let (bytes, _report) = Redactor::new_default().redact(value.as_bytes());
    String::from_utf8_lossy(bytes.as_ref()).into_owned()
}

/// Recursively redact every string leaf of a JSON value (used for the
/// skill-events array so anchors / names are scrubbed uniformly).
fn redact_extracted_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(redact_extracted_string(&text))
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(redact_extracted_json).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, redact_extracted_json(v)))
                .collect(),
        ),
        other => other,
    }
}

/// AG-21: run the adapter's transcript-intelligence capabilities over the
/// raw transcript and shape the result for `metadata.json`'s additive
/// `extraction` object. Fail-open by construction:
///
/// - no adapter / no raw transcript → `{present:false, partial:true}` with
///   an explanatory (non-sensitive) warning;
/// - individual extractor failure → warning + `partial:true`, remaining
///   dimensions still recorded;
/// - warnings pass through the default `Redactor` before persistence so a
///   pathological error string can never leak transcript content.
///
/// Prompt TEXT is deliberately NOT persisted here — only the prompt count.
/// The redacted transcript blob remains the sole persisted content
/// carrier; extraction stores derived, low-sensitivity facts (usage
/// numbers, model id, file paths, curated skill events).
fn build_extraction_metadata(agent_kind: &str, raw: Option<&[u8]>) -> serde_json::Value {
    use crate::internal::ai::observed_agents::{AgentKind, agent_for};

    let mut partial = false;
    let mut warnings: Vec<String> = Vec::new();
    let mut value = serde_json::json!({
        "schema_version": 1,
        "present": false,
        "partial": false,
        "warnings": [],
    });

    let adapter = AgentKind::from_db_str(agent_kind).map(agent_for);
    let (Some(adapter), Some(raw)) = (adapter, raw) else {
        partial = true;
        warnings.push(if adapter.is_none() {
            format!("unknown agent kind '{agent_kind}'; extraction skipped")
        } else {
            "no raw transcript available; extraction skipped".to_string()
        });
        finalize_extraction(&mut value, partial, warnings);
        return value;
    };

    let object = value
        .as_object_mut()
        .expect("extraction value is an object");
    object.insert("present".into(), serde_json::Value::Bool(true));

    if let Some(calculator) = adapter.as_token_calculator() {
        match calculator.calculate_token_usage(raw, 0) {
            Ok(usage) => {
                object.insert(
                    "token_usage".into(),
                    serde_json::to_value(&usage).unwrap_or(serde_json::Value::Null),
                );
            }
            Err(err) => {
                partial = true;
                warnings.push(format!("token usage extraction failed: {err:#}"));
            }
        }
    }
    if let Some(extractor) = adapter.as_model_extractor() {
        match extractor.extract_model(raw) {
            Ok(Some(model)) => {
                object.insert(
                    "model".into(),
                    serde_json::Value::String(redact_extracted_string(&model)),
                );
            }
            Ok(None) => {}
            Err(err) => {
                partial = true;
                warnings.push(format!("model extraction failed: {err:#}"));
            }
        }
    }
    if let Some(extractor) = adapter.as_prompt_extractor() {
        match extractor.extract_prompts(raw, 0) {
            Ok(prompts) => {
                object.insert("prompt_count".into(), serde_json::json!(prompts.len()));
            }
            Err(err) => {
                partial = true;
                warnings.push(format!("prompt extraction failed: {err:#}"));
            }
        }
    }
    if let Some(extractor) = adapter.as_subagent_aware_extractor() {
        match extractor.total_token_usage_including_subagents(raw) {
            Ok(usage) => {
                object.insert(
                    "subagent_token_usage".into(),
                    serde_json::to_value(&usage).unwrap_or(serde_json::Value::Null),
                );
            }
            Err(err) => {
                partial = true;
                warnings.push(format!("subagent usage extraction failed: {err:#}"));
            }
        }
    }
    if let Some(analyzer) = adapter.as_transcript_analyzer() {
        match analyzer.extract_modified_files_from_offset(raw, 0) {
            Ok(files) => {
                // File paths are derived from untrusted tool_use input —
                // redact them before persistence (a path can embed a
                // secret, e.g. a token in a URL-like path segment).
                let list: Vec<String> = files
                    .iter()
                    .map(|path| redact_extracted_string(&path.display().to_string()))
                    .collect();
                object.insert(
                    "modified_files".into(),
                    serde_json::to_value(list).unwrap_or(serde_json::Value::Null),
                );
            }
            Err(err) => {
                partial = true;
                warnings.push(format!("modified-files extraction failed: {err:#}"));
            }
        }
    }
    if let Some(extractor) = adapter.as_skill_event_extractor() {
        match extractor.extract_skill_events(raw, 0) {
            Ok(events) => {
                // Skill events carry curated slash-command names + opaque
                // anchors; redact the serialized form defensively so any
                // transcript-derived string is scrubbed uniformly.
                let value = serde_json::to_value(&events).unwrap_or(serde_json::Value::Null);
                object.insert("skill_events".into(), redact_extracted_json(value));
            }
            Err(err) => {
                partial = true;
                warnings.push(format!("skill event extraction failed: {err:#}"));
            }
        }
    }

    // The per-format parsers may themselves flag partial results (e.g.
    // undecodable lines) — surface their warnings through the same
    // channel. They live on the summary the extractors already consumed;
    // recompute cheaply through the analyzer-agnostic path.
    let format_summary = match AgentKind::from_db_str(agent_kind) {
        Some(AgentKind::ClaudeCode) => {
            Some(crate::internal::ai::observed_agents::extract::extract_claude_code(raw))
        }
        Some(AgentKind::Codex) => {
            Some(crate::internal::ai::observed_agents::extract::extract_codex(raw))
        }
        Some(AgentKind::OpenCode) => {
            Some(crate::internal::ai::observed_agents::extract::extract_opencode(raw))
        }
        _ => None,
    };
    if let Some(summary) = format_summary {
        if summary.partial {
            partial = true;
        }
        // Additive E6 count fields (numbers only — no redaction needed).
        object.insert(
            "api_call_count".into(),
            serde_json::json!(summary.api_call_count),
        );
        // The generic E6 path (codex/opencode) folds the wire
        // `subagent_tokens` into `subagent_usage` but exposes no
        // `SubagentAwareExtractor`, so persist it here when the
        // accessor block above did not already write it.
        if !object.contains_key("subagent_token_usage")
            && let Some(subagent) = &summary.subagent_usage
        {
            object.insert(
                "subagent_token_usage".into(),
                serde_json::to_value(subagent).unwrap_or(serde_json::Value::Null),
            );
        }
        warnings.extend(summary.warnings);
    }

    finalize_extraction(&mut value, partial, warnings);
    value
}

/// Stamp `partial` + redacted warnings onto the extraction object.
fn finalize_extraction(value: &mut serde_json::Value, partial: bool, warnings: Vec<String>) {
    use crate::internal::ai::observed_agents::Redactor;
    let redactor = Redactor::new_default();
    let redacted: Vec<String> = warnings
        .into_iter()
        .map(|warning| {
            let (bytes, _report) = redactor.redact(warning.as_bytes());
            String::from_utf8_lossy(bytes.as_ref()).into_owned()
        })
        .collect();
    if let Some(object) = value.as_object_mut() {
        object.insert("partial".into(), serde_json::Value::Bool(partial));
        object.insert(
            "warnings".into(),
            serde_json::to_value(redacted).unwrap_or_else(|_| serde_json::json!([])),
        );
    }
}

fn checkpoint_model_field(model: Option<&serde_json::Value>) -> String {
    match model {
        Some(serde_json::Value::String(name)) if !name.trim().is_empty() => name.clone(),
        Some(serde_json::Value::Object(obj)) => ["id", "model", "name", "display_name"]
            .iter()
            .find_map(|key| obj.get(*key).and_then(serde_json::Value::as_str))
            .filter(|name| !name.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string()),
        _ => "unknown".to_string(),
    }
}

/// Merge a [`RedactionReport`](crate::internal::ai::observed_agents::RedactionReport)
/// produced while redacting the captured transcript into the checkpoint's
/// existing `redaction_report` JSON object (built from the event payload's
/// prompt / tool-input matches). Appends the transcript's `matches` and adds
/// its `bytes_scanned` / `bytes_redacted` counters so the stored report stays
/// consistent with the stored (redacted) transcript blob. A non-object
/// `report` (only possible from a malformed input string) is left untouched.
/// Decide whether `path` may be read into a checkpoint transcript blob.
///
/// The transcript path originates from the (untrusted) hook envelope, so a
/// forged payload could otherwise point it at any file the Libra process can
/// read and have the contents copied into the syncable `traces` blob
/// (entire.md §8.1 / §13 P0). Constrain it: after symlink canonicalization,
/// `path` must live under one of the adapter's home-relative roots (e.g.
/// `~/.claude` for Claude Code, `~/.gemini` for Gemini). Non-existent paths,
/// an unresolvable home directory, or a path outside every root all return
/// `false` so the caller falls back to the already-redacted prompt.
/// `LIBRA_TEST_HOME` overrides the home directory for tests, mirroring the
/// vault module.
fn transcript_path_within_provider_root(
    adapter: &dyn crate::internal::ai::observed_agents::ObservedAgent,
    path: &std::path::Path,
) -> bool {
    let home = std::env::var_os("LIBRA_TEST_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir);
    let Some(home) = home else {
        return false;
    };
    let Ok(canonical_path) = path.canonicalize() else {
        return false;
    };
    adapter.protected_dirs().iter().any(|dir| {
        // Codex relocates its whole home via `$CODEX_HOME` and every other
        // part of the codex chain honors it (`resolve_codex_home` in
        // providers/codex/settings.rs). The trust gate must resolve the
        // same root, or sessions under a relocated CODEX_HOME are silently
        // captured with empty transcripts (found by the A6.5 real-CLI
        // smoke, which isolates CODEX_HOME). Only an absolute override is
        // honored, mirroring `resolve_codex_home`'s validation.
        let root = if *dir == ".codex" {
            match std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from) {
                Some(path) if path.is_absolute() => path,
                _ => home.join(dir),
            }
        } else {
            home.join(dir)
        };
        root.canonicalize()
            .map(|root| canonical_path.starts_with(root))
            .unwrap_or(false)
    })
}

fn merge_redaction_report_into(
    report: &mut serde_json::Value,
    extra: &crate::internal::ai::observed_agents::RedactionReport,
) {
    let Some(obj) = report.as_object_mut() else {
        return;
    };
    if !extra.matches.is_empty() {
        let extra_matches =
            serde_json::to_value(&extra.matches).unwrap_or_else(|_| serde_json::json!([]));
        match obj.get_mut("matches").and_then(|m| m.as_array_mut()) {
            Some(arr) => {
                if let Some(extra_arr) = extra_matches.as_array() {
                    arr.extend(extra_arr.iter().cloned());
                }
            }
            None => {
                obj.insert("matches".to_string(), extra_matches);
            }
        }
    }
    for (key, added) in [
        ("bytes_scanned", extra.bytes_scanned),
        ("bytes_redacted", extra.bytes_redacted),
    ] {
        let current = obj
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        obj.insert(key.to_string(), serde_json::json!(current + added as u64));
    }
}

///
/// Mirrors `cli::set_local_hash_kind_for_storage` but reads via the already-open
/// connection that the hook runtime obtains. Defaults to `sha1` for repositories
/// initialised before SHA-256 support landed.
async fn set_hash_kind_from_repo() -> Result<()> {
    let object_format = ConfigKv::get("core.objectformat")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| "sha1".to_string());

    let hash_kind = match object_format.as_str() {
        "sha1" => HashKind::Sha1,
        "sha256" => HashKind::Sha256,
        _ => bail!("unsupported object format: '{object_format}'"),
    };
    set_hash_kind(hash_kind);
    Ok(())
}

/// Apply the canonical event together with bookkeeping into `session`.
///
/// Functional scope: bumps `updated_at`, records the transcript path if any,
/// appends the raw envelope to the audit ring, applies the lifecycle delta, and
/// transitions the coarse phase. Finally appends a normalized projection-friendly
/// fragment to `normalized_events` so downstream consumers don't re-parse the raw
/// envelope.
fn apply_hook_event(
    session: &mut SessionState,
    envelope: &SessionHookEnvelope,
    event: &LifecycleEvent,
    provider_name: &str,
) {
    session.updated_at = Utc::now();

    if let Some(session_ref) = &event.session_ref {
        session.metadata.insert(
            "transcript_path".to_string(),
            Value::String(session_ref.clone()),
        );
    }

    append_raw_hook_event(session, envelope, MAX_RAW_HOOK_EVENTS);
    apply_lifecycle_event(session, event, MAX_TOOL_EVENTS);
    transition_phase(session, event.kind);
    append_normalized_event(session, event, provider_name);
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
pub(crate) fn append_normalized_event(
    session: &mut SessionState,
    event: &LifecycleEvent,
    provider_name: &str,
) {
    let entry = session
        .metadata
        .entry(NORMALIZED_EVENTS_KEY.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));

    let normalized = json!({
        "provider": provider_name,
        "kind": event.kind.to_string(),
        "timestamp": event.timestamp.to_rfc3339(),
        "prompt": event.prompt,
        "tool_name": event.tool_name,
        "assistant_message": event.assistant_message,
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
/// Boundary conditions: any I/O error short-circuits with context; the caller
/// catches and surfaces it via session metadata so the user sees an actionable
/// message and can retry.
async fn persist_session_history(
    storage_path: &Path,
    session: &SessionState,
    provider: &dyn HookProvider,
) -> Result<PersistOutcome> {
    let objects_dir = storage_path.join("objects");
    std::fs::create_dir_all(&objects_dir)?;

    let storage = Arc::new(ClientStorage::init(objects_dir));
    let db_conn = Arc::new(db::get_db_conn_instance().await.clone());
    let history_manager = HistoryManager::new(storage, storage_path.to_path_buf(), db_conn);

    if let Some(existing) = history_manager
        .get_object_hash(AI_SESSION_TYPE, &session.id)
        .await?
    {
        return Ok(PersistOutcome {
            object_hash: existing.to_string(),
            already_exists: true,
        });
    }

    let payload = build_ai_session_payload(session, provider);
    let blob_data = serde_json::to_vec(&normalize_json_value(payload))
        .context("failed to serialize ai_session payload")?;
    let blob_hash = write_git_object(storage_path, "blob", &blob_data)?;
    history_manager
        .append(AI_SESSION_TYPE, &session.id, blob_hash)
        .await?;

    Ok(PersistOutcome {
        object_hash: blob_hash.to_string(),
        already_exists: false,
    })
}

/// Construct the canonical JSON payload persisted as an `ai_session` blob.
///
/// Functional scope: bundles a state-machine summary, a message-count summary, the
/// transcript pointer, the projected event stream, the raw event ring, and the
/// in-memory session itself. The whole document is keyed by the
/// [`AI_SESSION_SCHEMA`] string so future schema migrations can detect old blobs.
fn build_ai_session_payload(session: &SessionState, provider: &dyn HookProvider) -> Value {
    let events = session
        .metadata
        .get(NORMALIZED_EVENTS_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let raw_events = session
        .metadata
        .get("raw_hook_events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
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
    let transcript_path = session
        .metadata
        .get("transcript_path")
        .and_then(Value::as_str);
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
            "history_ref": AI_REF,
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
    use serial_test::serial;

    use super::*;
    use crate::internal::ai::hooks::providers::{claude_provider, gemini_provider};

    /// AG-21 metadata persistence (codex review R2 P1): the generic E6
    /// path (codex/opencode) must persist `subagent_token_usage` and
    /// `api_call_count` into the checkpoint metadata `extraction` block —
    /// not just into the in-memory summary. Drives the exact
    /// `build_extraction_metadata` path a checkpoint write uses.
    #[test]
    fn extraction_metadata_persists_generic_subagent_and_api_count() {
        let transcript = concat!(
            r#"{"role":"user","content":"/review"}"#,
            "\n",
            r#"{"model":"gpt-5.3-codex","usage":{"input_tokens":10,"output_tokens":4,"api_call_count":5,"subagent_tokens":30}}"#,
            "\n",
        );
        let value = build_extraction_metadata("codex", Some(transcript.as_bytes()));
        let extraction = value.as_object().expect("extraction object");
        assert_eq!(extraction["present"], serde_json::json!(true));
        assert_eq!(
            extraction["api_call_count"],
            serde_json::json!(5),
            "wire api_call_count persisted (not +1): {value}"
        );
        let subagent = &extraction["subagent_token_usage"];
        assert_eq!(
            subagent["input_tokens"],
            serde_json::json!(30),
            "generic-path subagent tokens persisted: {value}"
        );

        // Claude uses the SubagentAwareExtractor accessor — its
        // subagent_token_usage must be written by that block and NOT
        // double-written by the generic fallback (value stays the
        // accessor's, and the key exists exactly once in a JSON object).
        let claude_line = serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "model": "claude-sonnet-5",
                "content": [{"type": "tool_use", "name": "Task", "input": {"prompt": "x"}}],
                "usage": {"input_tokens": 7, "output_tokens": 2}
            }
        });
        let claude =
            build_extraction_metadata("claude_code", Some(format!("{claude_line}\n").as_bytes()));
        assert!(
            claude["subagent_token_usage"].is_object(),
            "claude subagent usage present via accessor: {claude}"
        );
    }

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

    // Scenario: the same envelope yields identical dedup keys regardless of
    // which provider's identity-key list is supplied, because both lists pull
    // from `CANONICAL_DEDUP_IDENTITY_KEYS`.
    #[test]
    fn dedup_keys_remain_stable_across_providers() {
        let envelope = SessionHookEnvelope {
            hook_event_name: "UserPromptSubmit".to_string(),
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: None,
            extra: {
                let mut map = Map::new();
                map.insert("event_id".to_string(), Value::String("evt-1".to_string()));
                map
            },
        };

        let claude_key = make_dedup_key(
            claude_provider().dedup_identity_keys(),
            claude_provider().lifecycle_fallback_events(),
            &envelope,
        );
        let gemini_key = make_dedup_key(
            gemini_provider().dedup_identity_keys(),
            gemini_provider().lifecycle_fallback_events(),
            &envelope,
        );
        assert_eq!(claude_key, gemini_key);
    }

    // Scenario: identical native session IDs from different providers do not
    // collide because the namespacing prefix differs.
    #[test]
    fn session_id_is_namespaced_by_provider() {
        assert_eq!(
            build_ai_session_id("gemini", "session-123"),
            "gemini__session-123"
        );
        assert_eq!(
            build_ai_session_id("claude", "session-123"),
            "claude__session-123"
        );
    }

    // Scenario: long IDs keep their first eight characters; short IDs are fully
    // masked.
    #[test]
    fn session_id_redaction_masks_suffix() {
        assert_eq!(redact_session_id("gemini__session-123"), "gemini__***");
        assert_eq!(redact_session_id("short"), "***");
    }

    // Scenario: a synthetic ended session includes the schema id, state machine
    // counters, message-count summary, and transcript path in the payload.
    #[test]
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
        assert_eq!(payload["transcript"]["path"], json!("/tmp/t.jsonl"));
    }

    // -------------------------------------------------------------------
    // CEX-EntireIO: AgentTraces ingest tests. Codex round-2 BLOCK #10 + #2
    // round-3 followup ("assert observable redaction outcome").
    // -------------------------------------------------------------------

    use sea_orm::{
        ConnectOptions, ConnectionTrait, Database, DatabaseConnection, ExecResult, Statement,
    };
    use tempfile::TempDir;

    use crate::internal::db::{
        ensure_ai_runtime_contract_schema, migration::run_builtin_migrations,
    };

    const LEGACY_BOOTSTRAP_SQL: &str = include_str!("../../../../sql/sqlite_20260309_init.sql");

    async fn ingest_fresh_conn() -> (TempDir, DatabaseConnection) {
        let dir = tempfile::tempdir().expect("tempdir");
        // Use the canonical `libra.db` filename here so the Phase 3.5c
        // object_index queue (`enqueue_agent_blob_object_index_update`)
        // — which derives the database path from `repo_path.join(DATABASE)`
        // — finds the same file the test fixture set up.
        let path = dir.path().join(crate::utils::util::DATABASE);
        std::fs::File::create(&path).expect("touch sqlite file");
        let url = format!("sqlite://{}", path.display());
        let mut opts = ConnectOptions::new(url);
        opts.sqlx_logging(false);
        let conn = Database::connect(opts).await.expect("connect");
        // Mirror production wiring exactly: legacy bootstrap (creates
        // `ai_thread`) → AI runtime contract → registered migrations.
        let backend = conn.get_database_backend();
        for raw in LEGACY_BOOTSTRAP_SQL.split(';') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let _: ExecResult = conn
                .execute(Statement::from_string(backend, trimmed.to_string()))
                .await
                .unwrap_or_else(|e| panic!("legacy bootstrap stmt failed: {trimmed}\n{e}"));
        }
        ensure_ai_runtime_contract_schema(&conn)
            .await
            .expect("ensure_ai_runtime_contract_schema");
        run_builtin_migrations(&conn)
            .await
            .expect("run_builtin_migrations");
        (dir, conn)
    }

    fn ingest_envelope(
        hook_event_name: &str,
        session_id: &str,
        extra: serde_json::Value,
    ) -> Vec<u8> {
        let mut base = json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": "/tmp/repo",
            "transcript_path": "/tmp/repo/transcript.jsonl",
        });
        if let serde_json::Value::Object(extra_map) = extra
            && let serde_json::Value::Object(base_map) = &mut base
        {
            for (k, v) in extra_map {
                base_map.insert(k, v);
            }
        }
        serde_json::to_vec(&base).expect("serialize envelope")
    }

    #[tokio::test]
    async fn ingest_session_start_creates_active_row() {
        let (_dir, conn) = ingest_fresh_conn().await;

        let payload = ingest_envelope("SessionStart", "S-001", json!({}));
        ingest_agent_traces_payload(
            &payload,
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session start ingest succeeds");

        let backend = conn.get_database_backend();
        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT agent_kind, state, working_dir, provider_session_id, stopped_at \
                 FROM agent_session WHERE provider_session_id = ?",
                ["S-001".into()],
            ))
            .await
            .expect("query")
            .expect("session row exists");

        assert_eq!(
            row.try_get_by::<String, _>("agent_kind").unwrap(),
            "claude_code"
        );
        assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "active");
        assert_eq!(
            row.try_get_by::<String, _>("working_dir").unwrap(),
            "/tmp/repo"
        );
        assert_eq!(
            row.try_get_by::<String, _>("provider_session_id").unwrap(),
            "S-001"
        );
        assert!(
            row.try_get_by::<Option<i64>, _>("stopped_at")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn ingest_session_end_marks_stopped_and_is_idempotent() {
        let (_dir, conn) = ingest_fresh_conn().await;

        let start = ingest_envelope("SessionStart", "S-002", json!({}));
        ingest_agent_traces_payload(
            &start,
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("start ok");

        let end = ingest_envelope("SessionEnd", "S-002", json!({}));
        ingest_agent_traces_payload(
            &end,
            super::super::provider::ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("end ok");

        let backend = conn.get_database_backend();
        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT state, stopped_at FROM agent_session WHERE provider_session_id = ?",
                ["S-002".into()],
            ))
            .await
            .expect("query")
            .expect("row");

        assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
        assert!(
            row.try_get_by::<Option<i64>, _>("stopped_at")
                .unwrap()
                .is_some()
        );

        // Repeat-ingest is idempotent: still exactly one row for that session.
        let count_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS n FROM agent_session WHERE provider_session_id = ?",
                ["S-002".into()],
            ))
            .await
            .expect("count query")
            .expect("count row");
        assert_eq!(count_row.try_get_by::<i64, _>("n").unwrap(), 1);
    }

    /// Round-3 strengthened test: the redaction_report column should be
    /// populated with at least one match when an envelope carries a known
    /// secret, so the persisted row carries observable evidence the redactor
    /// ran.
    #[tokio::test]
    async fn ingest_persists_observable_redaction_report() {
        let (_dir, conn) = ingest_fresh_conn().await;

        let payload = ingest_envelope(
            "UserPromptSubmit",
            "S-redact",
            json!({
                "prompt": "deploy with AKIAIOSFODNN7EXAMPLE please",
            }),
        );
        ingest_agent_traces_payload(
            &payload,
            super::super::provider::ProviderHookCommand::Prompt,
            LifecycleEventKind::TurnStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("prompt ingest succeeds");

        let backend = conn.get_database_backend();
        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT state, redaction_report FROM agent_session WHERE provider_session_id = ?",
                ["S-redact".into()],
            ))
            .await
            .expect("query")
            .expect("row");

        assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "active");

        let report_json: String = row.try_get_by("redaction_report").unwrap();
        let report: serde_json::Value =
            serde_json::from_str(&report_json).expect("redaction_report is JSON");
        let matches = report
            .get("matches")
            .and_then(|v| v.as_array())
            .expect("matches is an array");
        assert!(
            !matches.is_empty(),
            "redaction_report.matches must be non-empty when prompt carries a known secret; got: {report_json}"
        );
        let bytes_redacted = report
            .get("bytes_redacted")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert!(
            bytes_redacted > 0,
            "redaction_report.bytes_redacted must be > 0; got {bytes_redacted}"
        );
        // The literal AKIA secret must NOT be reachable through the
        // persisted row — the only place we'd have stored its bytes is the
        // redaction_report, which now contains only positional matches.
        assert!(
            !report_json.contains("AKIAIOSFODNN7EXAMPLE"),
            "raw secret leaked into redaction_report column: {report_json}"
        );
    }

    /// Read the `metadata_json` for a provider session id as a JSON value.
    async fn ingest_session_metadata(
        conn: &DatabaseConnection,
        provider_session_id: &str,
    ) -> serde_json::Value {
        let backend = conn.get_database_backend();
        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT metadata_json FROM agent_session WHERE provider_session_id = ?",
                [provider_session_id.into()],
            ))
            .await
            .expect("metadata query")
            .expect("session row");
        let json: String = row.try_get_by("metadata_json").unwrap();
        serde_json::from_str(&json).expect("metadata_json is valid JSON")
    }

    /// §6.3 state machine: a second concurrent session that submits a prompt
    /// (`TurnStart`) while a peer is still `active` in the same `working_dir`
    /// records `concurrent_active=true` in its session metadata, and a session
    /// that never observed a peer at a turn stays unmarked.
    #[tokio::test]
    async fn ingest_turn_start_marks_concurrent_active_with_peer_in_same_workdir() {
        let (_dir, conn) = ingest_fresh_conn().await;

        // Session A starts and stays active (shared cwd `/tmp/repo`).
        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", "S-A", json!({})),
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session A start ingest succeeds");

        // Session B starts in the same working_dir.
        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", "S-B", json!({})),
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session B start ingest succeeds");

        // Session B submits a prompt → must observe A and flag concurrency.
        ingest_agent_traces_payload(
            &ingest_envelope("UserPromptSubmit", "S-B", json!({ "prompt": "hello" })),
            super::super::provider::ProviderHookCommand::Prompt,
            LifecycleEventKind::TurnStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session B prompt ingest succeeds");

        let b_meta = ingest_session_metadata(&conn, "S-B").await;
        assert_eq!(
            b_meta.get("concurrent_active").and_then(|v| v.as_bool()),
            Some(true),
            "B's TurnStart with an active peer must record concurrent_active=true: {b_meta}"
        );

        // A never ran a turn alongside a peer, so it stays unmarked.
        let a_meta = ingest_session_metadata(&conn, "S-A").await;
        assert!(
            a_meta.get("concurrent_active").is_none(),
            "A had no concurrent turn and must not be flagged: {a_meta}"
        );
    }

    /// A lone session's `TurnStart` with no peer active in the same
    /// `working_dir` must not raise `concurrent_active`.
    #[tokio::test]
    async fn ingest_turn_start_without_peer_does_not_mark_concurrent_active() {
        let (_dir, conn) = ingest_fresh_conn().await;

        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", "S-solo", json!({})),
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("solo session start ingest succeeds");

        ingest_agent_traces_payload(
            &ingest_envelope("UserPromptSubmit", "S-solo", json!({ "prompt": "hi" })),
            super::super::provider::ProviderHookCommand::Prompt,
            LifecycleEventKind::TurnStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("solo session prompt ingest succeeds");

        let meta = ingest_session_metadata(&conn, "S-solo").await;
        assert!(
            meta.get("concurrent_active").is_none(),
            "a lone session must not be flagged concurrent_active: {meta}"
        );
    }

    /// Once a turn marks a session `concurrent_active`, the marker is sticky:
    /// later events merge (`json_patch`) into the existing metadata rather
    /// than overwriting it, so a subsequent turn whose envelope no longer has
    /// a peer (and so omits the flag) cannot clear the marker, and updating
    /// another key (`transcript_path`) preserves it.
    #[tokio::test]
    async fn ingest_metadata_merge_preserves_marker_and_updates_transcript() {
        let (_dir, conn) = ingest_fresh_conn().await;

        for sid in ["S-A", "S-B"] {
            ingest_agent_traces_payload(
                &ingest_envelope("SessionStart", sid, json!({})),
                super::super::provider::ProviderHookCommand::SessionStart,
                LifecycleEventKind::SessionStart,
                claude_provider(),
                &conn,
                None,
            )
            .await
            .expect("session start ingest succeeds");
        }

        // B's first turn observes A and is marked.
        ingest_agent_traces_payload(
            &ingest_envelope("UserPromptSubmit", "S-B", json!({ "prompt": "hello" })),
            super::super::provider::ProviderHookCommand::Prompt,
            LifecycleEventKind::TurnStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session B first prompt ingest succeeds");

        // A stops, so no peer remains active.
        ingest_agent_traces_payload(
            &ingest_envelope("SessionEnd", "S-A", json!({})),
            super::super::provider::ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session A end ingest succeeds");

        // B's second turn (peer gone) carries a *different* transcript_path
        // and omits the flag. The merge must update transcript_path while
        // keeping concurrent_active=true.
        let second_turn = json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "S-B",
            "cwd": "/tmp/repo",
            "transcript_path": "/tmp/repo/transcript-2.jsonl",
            "prompt": "second turn",
        });
        ingest_agent_traces_payload(
            &serde_json::to_vec(&second_turn).expect("serialize envelope"),
            super::super::provider::ProviderHookCommand::Prompt,
            LifecycleEventKind::TurnStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session B second prompt ingest succeeds");

        let meta = ingest_session_metadata(&conn, "S-B").await;
        assert_eq!(
            meta.get("concurrent_active").and_then(|v| v.as_bool()),
            Some(true),
            "marker must survive a later peer-free turn via metadata merge: {meta}"
        );
        assert_eq!(
            meta.get("transcript_path").and_then(|v| v.as_str()),
            Some("/tmp/repo/transcript-2.jsonl"),
            "the later turn's transcript_path must be merged in: {meta}"
        );
    }

    #[tokio::test]
    async fn ingest_rejects_kind_mismatch() {
        let (_dir, conn) = ingest_fresh_conn().await;

        let payload = ingest_envelope("SessionStart", "S-mismatch", json!({}));
        let err = ingest_agent_traces_payload(
            &payload,
            super::super::provider::ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect_err("kind mismatch must fail");
        assert!(
            err.to_string().contains("hook event kind mismatch"),
            "unexpected error: {err}"
        );

        // No row should have been written.
        let backend = conn.get_database_backend();
        let count_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS n FROM agent_session WHERE provider_session_id = ?",
                ["S-mismatch".into()],
            ))
            .await
            .expect("count query")
            .expect("count row");
        assert_eq!(count_row.try_get_by::<i64, _>("n").unwrap(), 0);
    }

    #[tokio::test]
    async fn ingest_fails_loud_when_table_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("noschema.db");
        std::fs::File::create(&path).expect("touch sqlite file");
        let url = format!("sqlite://{}", path.display());
        let mut opts = ConnectOptions::new(url);
        opts.sqlx_logging(false);
        let conn = Database::connect(opts).await.expect("connect");
        // intentionally NOT calling run_builtin_migrations.

        let payload = ingest_envelope("SessionStart", "S-bare", json!({}));
        let err = ingest_agent_traces_payload(
            &payload,
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect_err("missing table must fail");
        assert!(
            err.to_string()
                .contains("agent_session table does not exist"),
            "unexpected error: {err}"
        );
    }

    /// entire.md §6.3: a `TurnEnd` (Stop) event with a `repo_path` must also
    /// materialise a `committed` checkpoint (per-turn rewind granularity)
    /// while leaving the session `active` — checkpoints are no longer
    /// SessionEnd-only.
    #[tokio::test]
    async fn ingest_turn_end_writes_committed_checkpoint() {
        let (dir, conn) = ingest_fresh_conn().await;
        let repo_path = dir.path().to_path_buf();

        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", "S-turn-cp", json!({})),
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start ok");

        // TurnEnd (Stop): the session stays active, but a committed checkpoint
        // must be written for the turn.
        ingest_agent_traces_payload(
            &ingest_envelope("Stop", "S-turn-cp", json!({})),
            super::super::provider::ProviderHookCommand::Stop,
            LifecycleEventKind::TurnEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("turn end ok");

        let backend = conn.get_database_backend();
        let state_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT state FROM agent_session WHERE provider_session_id = 'S-turn-cp'",
                [],
            ))
            .await
            .expect("state query")
            .expect("session row");
        assert_eq!(
            state_row.try_get_by::<String, _>("state").unwrap(),
            "active",
            "a TurnEnd must not stop the session"
        );

        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT scope, traces_commit FROM agent_checkpoint \
                 WHERE session_id = (SELECT session_id FROM agent_session \
                   WHERE provider_session_id = 'S-turn-cp' LIMIT 1)",
                [],
            ))
            .await
            .expect("checkpoint query")
            .expect("a committed checkpoint must exist for the TurnEnd");
        assert_eq!(row.try_get_by::<String, _>("scope").unwrap(), "committed");
        assert!(
            !row.try_get_by::<String, _>("traces_commit")
                .unwrap()
                .is_empty(),
            "checkpoint must reference a non-empty traces commit"
        );
    }

    /// Phase 2.1: when a `repo_path` is supplied and SessionEnd fires, the
    /// runtime must (a) write a checkpoint commit on `refs/libra/traces`
    /// and (b) insert a row into `agent_checkpoint`. The checkpoint blob /
    /// commit objects live under `<repo>/objects/`, so we point the test at a
    /// fresh tempdir for that side too.
    #[tokio::test]
    async fn ingest_session_end_writes_checkpoint_when_repo_path_provided() {
        let (dir, conn) = ingest_fresh_conn().await;
        // Use the same tempdir as the SQLite file so the objects directory
        // and DB live together. We never need to run `libra init` here —
        // `append_checkpoint_commit` only needs the objects/ directory and
        // a sea-orm connection.
        let repo_path = dir.path().to_path_buf();

        let start = ingest_envelope("SessionStart", "S-cp", json!({}));
        ingest_agent_traces_payload(
            &start,
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start ok");

        // SessionEnd must trigger checkpoint creation.
        let end = ingest_envelope("SessionEnd", "S-cp", json!({}));
        ingest_agent_traces_payload(
            &end,
            super::super::provider::ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("end ok");

        let backend = conn.get_database_backend();
        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id, scope, traces_commit, tree_oid, metadata_blob_oid \
                 FROM agent_checkpoint WHERE session_id = (SELECT session_id FROM agent_session \
                  WHERE provider_session_id = 'S-cp' LIMIT 1)",
                [],
            ))
            .await
            .expect("query")
            .expect("checkpoint row exists");
        assert_eq!(row.try_get_by::<String, _>("scope").unwrap(), "committed");
        let traces_commit: String = row.try_get_by("traces_commit").unwrap();
        let tree_oid: String = row.try_get_by("tree_oid").unwrap();
        let metadata_blob_oid: String = row.try_get_by("metadata_blob_oid").unwrap();
        assert!(!traces_commit.is_empty());
        assert!(!tree_oid.is_empty());
        assert!(!metadata_blob_oid.is_empty());

        // The metadata blob must exist on disk and parse as JSON whose
        // `agent_kind` matches what we ingested.
        let metadata_path = repo_path
            .join("objects")
            .join(&metadata_blob_oid[..2])
            .join(&metadata_blob_oid[2..]);
        assert!(
            metadata_path.exists(),
            "metadata blob missing at {metadata_path:?}"
        );

        // The traces ref row must point at the checkpoint commit hash.
        let backend = conn.get_database_backend();
        let ref_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT `commit` FROM reference WHERE name = ? AND kind = 'Branch' LIMIT 1",
                [crate::internal::branch::TRACES_BRANCH.into()],
            ))
            .await
            .expect("query traces ref")
            .expect("traces ref row exists");
        let head: String = ref_row.try_get_by("commit").unwrap();
        assert_eq!(head, traces_commit);

        // Phase 3.5c acceptance: every object touched by the agent
        // capture history must be tagged in `object_index` so cloud sync
        // uploads them. Without this, `libra cloud restore` would
        // resolve the orphan ref's commits to missing trees/blobs on a
        // fresh clone. The transcript blob carries the distinguished
        // `agent_transcript` o_type per entire.md §14.3.
        //
        // Codex round-1 follow-up: walk the *entire* reachability set
        // (commit → root tree → … → leaf blobs) rather than spot-
        // checking the root tree only. Walking the actual on-disk
        // objects catches new code paths that forget to call
        // `write_tree_indexed` for an intermediate tree.
        crate::utils::client_storage::ClientStorage::wait_for_background_tasks();

        verify_full_reachability_indexed(&conn, &repo_path, &traces_commit).await;

        // Spot check the metadata blob OID (Phase 3.5b's
        // `agent_checkpoint.metadata_blob_oid` column should join cleanly
        // to `object_index`).
        let metadata_count_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS n FROM object_index WHERE o_id = ?",
                [metadata_blob_oid.clone().into()],
            ))
            .await
            .expect("query metadata count")
            .expect("count row");
        let metadata_count: i64 = metadata_count_row.try_get_by("n").unwrap();
        assert_eq!(metadata_count, 1, "metadata blob is indexed");

        // Spot check the distinctive `agent_transcript` tag — at least
        // one row carries it (the transcript blob), per the spec.
        let transcript_count_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS n FROM object_index WHERE o_type = 'agent_transcript'",
                [],
            ))
            .await
            .expect("query agent_transcript count")
            .expect("count row");
        let transcript_count: i64 = transcript_count_row.try_get_by("n").unwrap();
        assert_eq!(
            transcript_count, 1,
            "exactly one transcript blob carries the agent_transcript o_type"
        );

        let _ = tree_oid; // silence unused warning — assertions above
        // already verified the root tree is indexed
        // via the reachability walker
    }

    /// entire.md §8.1 / §13 (P0) end-to-end: a SessionEnd prompt carrying
    /// a known secret must land in the `traces` transcript blob
    /// REDACTED — the raw secret never reaches durable storage. Guards the
    /// `RedactedBytes` write-path contract: the transcript blob is produced
    /// only via `RedactedBytes`, and the upstream redactor scrubbed the
    /// secret before it was wrapped. A regression that bypassed redaction
    /// (or the type) would surface here as the literal key in the blob.
    #[tokio::test]
    async fn session_end_checkpoint_transcript_blob_is_redacted() {
        let (dir, conn) = ingest_fresh_conn().await;
        let repo_path = dir.path().to_path_buf();

        let start = ingest_envelope("SessionStart", "S-cp-redact", json!({}));
        ingest_agent_traces_payload(
            &start,
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start ok");

        // SessionEnd whose prompt carries a known AWS-key-shaped secret.
        let end = ingest_envelope(
            "SessionEnd",
            "S-cp-redact",
            json!({ "prompt": "deploy with AKIAIOSFODNN7EXAMPLE please" }),
        );
        ingest_agent_traces_payload(
            &end,
            super::super::provider::ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("end ok");

        crate::utils::client_storage::ClientStorage::wait_for_background_tasks();

        // Locate the transcript blob via its distinguished o_type, then
        // read + zlib-decode it and strip the `blob <len>\0` header.
        let backend = conn.get_database_backend();
        let blob_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT o_id FROM object_index WHERE o_type = 'agent_transcript' LIMIT 1",
                [],
            ))
            .await
            .expect("query transcript blob")
            .expect("a transcript blob must be indexed");
        let blob_oid: String = blob_row.try_get_by("o_id").unwrap();

        let object_path = repo_path
            .join("objects")
            .join(&blob_oid[..2])
            .join(&blob_oid[2..]);
        let raw = std::fs::read(&object_path).expect("read transcript blob object");
        let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        let header_end = decoded
            .iter()
            .position(|&b| b == 0)
            .expect("blob object has a header terminator");
        let body = String::from_utf8_lossy(&decoded[header_end + 1..]);

        assert!(
            !body.contains("AKIAIOSFODNN7EXAMPLE"),
            "raw secret leaked into the persisted transcript blob: {body}",
        );
        assert!(
            body.contains("deploy with") && body.contains("please"),
            "the redacted transcript must retain the non-secret text, got: {body}",
        );
    }

    /// A6.5 regression: codex relocates its whole home via `$CODEX_HOME`
    /// and every other part of the codex chain honors it
    /// (`resolve_codex_home`), so the transcript trust gate must resolve
    /// the same root — otherwise sessions under a relocated CODEX_HOME are
    /// silently captured with empty transcripts (exactly what the A6.5
    /// real-CLI smoke observed with its isolated CODEX_HOME).
    #[test]
    #[serial]
    fn codex_transcript_root_honors_codex_home_override() {
        let adapter = crate::internal::ai::observed_agents::agent_for(
            crate::internal::ai::observed_agents::AgentKind::Codex,
        );
        let codex_home = tempfile::tempdir().expect("codex home tempdir");
        let sessions = codex_home.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let rollout = sessions.join("rollout-test.jsonl");
        std::fs::write(&rollout, "{}\n").unwrap();

        // Fake $HOME so the real ~/.codex can never accidentally match.
        let home = tempfile::tempdir().expect("fake home tempdir");
        let prior_home = std::env::var_os("LIBRA_TEST_HOME");
        let prior_codex = std::env::var_os("CODEX_HOME");
        // SAFETY: test-only env mutation, restored below; serialised via
        // #[serial] so it cannot race other env readers.
        unsafe {
            std::env::set_var("LIBRA_TEST_HOME", home.path());
            std::env::set_var("CODEX_HOME", codex_home.path());
        }
        let trusted = transcript_path_within_provider_root(adapter, &rollout);
        unsafe {
            std::env::remove_var("CODEX_HOME");
        }
        let untrusted = transcript_path_within_provider_root(adapter, &rollout);
        unsafe {
            match prior_codex {
                Some(value) => std::env::set_var("CODEX_HOME", value),
                None => std::env::remove_var("CODEX_HOME"),
            }
            match prior_home {
                Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                None => std::env::remove_var("LIBRA_TEST_HOME"),
            }
        }
        assert!(
            trusted,
            "a rollout under $CODEX_HOME must pass the provider-root gate"
        );
        assert!(
            !untrusted,
            "without the override the relocated rollout stays untrusted"
        );
    }

    /// entire.md §6.3 / §7.1: the SessionEnd checkpoint transcript blob must
    /// carry the agent's FULL on-disk transcript (read via the
    /// `ObservedAgent::read_transcript` adapter), not just the closing
    /// prompt, and that transcript must be redacted before storage. Writes a
    /// real transcript file with a unique marker plus a secret, points the
    /// envelope at it, and asserts the persisted blob contains the marker
    /// (proving full capture) with the secret scrubbed.
    #[tokio::test]
    #[serial]
    async fn session_end_checkpoint_captures_full_transcript_via_adapter() {
        let (dir, conn) = ingest_fresh_conn().await;
        let repo_path = dir.path().to_path_buf();

        // The transcript must live under the provider's home-relative root
        // (`~/.claude`) to pass the security trust check, so stand up a fake
        // HOME via LIBRA_TEST_HOME and place the file there. It carries content
        // the closing prompt does NOT contain plus an AWS-key-shaped secret
        // that must be redacted.
        let home = tempfile::tempdir().expect("fake home tempdir");
        let claude_dir = home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let transcript_path = claude_dir.join("session-transcript.jsonl");
        std::fs::write(
            &transcript_path,
            "user: kick off the deploy\nassistant: full-transcript-marker-9f3 with AKIAIOSFODNN7EXAMPLE\n",
        )
        .unwrap();
        let transcript_path_str = transcript_path.to_string_lossy().to_string();

        let envelope = |hook: &str, prompt: Option<&str>| -> Vec<u8> {
            let mut base = json!({
                "hook_event_name": hook,
                "session_id": "S-full-transcript",
                "cwd": "/tmp/repo",
                "transcript_path": transcript_path_str,
            });
            if let (Some(p), Some(obj)) = (prompt, base.as_object_mut()) {
                obj.insert("prompt".to_string(), json!(p));
            }
            serde_json::to_vec(&base).unwrap()
        };

        let prior_home = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: test-only env mutation, restored before the assertions;
        // serialised via #[serial] so it cannot race other env readers.
        unsafe {
            std::env::set_var("LIBRA_TEST_HOME", home.path());
        }

        ingest_agent_traces_payload(
            &envelope("SessionStart", None),
            super::super::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start ok");

        // Closing prompt deliberately omits the transcript marker so the test
        // can distinguish "captured the prompt" from "captured the transcript".
        ingest_agent_traces_payload(
            &envelope("SessionEnd", Some("wrap up now")),
            super::super::provider::ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("end ok");

        // Restore the env before the (env-independent) assertions below.
        unsafe {
            match prior_home {
                Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                None => std::env::remove_var("LIBRA_TEST_HOME"),
            }
        }

        crate::utils::client_storage::ClientStorage::wait_for_background_tasks();

        let backend = conn.get_database_backend();
        let blob_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT o_id FROM object_index WHERE o_type = 'agent_transcript' LIMIT 1",
                [],
            ))
            .await
            .expect("query transcript blob")
            .expect("a transcript blob must be indexed");
        let blob_oid: String = blob_row.try_get_by("o_id").unwrap();

        let object_path = repo_path
            .join("objects")
            .join(&blob_oid[..2])
            .join(&blob_oid[2..]);
        let raw = std::fs::read(&object_path).expect("read transcript blob object");
        let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        let header_end = decoded
            .iter()
            .position(|&b| b == 0)
            .expect("blob object has a header terminator");
        let body = String::from_utf8_lossy(&decoded[header_end + 1..]);

        assert!(
            body.contains("full-transcript-marker-9f3"),
            "checkpoint must capture the full transcript via the adapter, not just the prompt: {body}",
        );
        assert!(
            !body.contains("AKIAIOSFODNN7EXAMPLE"),
            "the secret in the transcript must be redacted before storage: {body}",
        );
        assert!(
            !body.contains("wrap up now"),
            "the full transcript should replace the prompt-only stopgap: {body}",
        );
    }

    /// Walk every object reachable from the checkpoint commit (commit →
    /// root tree → recursively trees/blobs) and assert each OID appears
    /// in `object_index`. Used to guard against future regressions where
    /// a write path forgets to route through `write_tree_indexed` or
    /// the indexing helper.
    async fn verify_full_reachability_indexed(
        conn: &DatabaseConnection,
        repo_path: &std::path::Path,
        commit_oid: &str,
    ) {
        let mut to_walk: Vec<(String, &'static str)> = vec![(commit_oid.to_string(), "commit")];
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some((oid, expected_type)) = to_walk.pop() {
            if !visited.insert(oid.clone()) {
                continue;
            }
            assert_object_index_has(conn, &oid, expected_type).await;
            // Read the on-disk Git object to discover its references.
            let object_path = repo_path.join("objects").join(&oid[..2]).join(&oid[2..]);
            let raw =
                std::fs::read(&object_path).unwrap_or_else(|e| panic!("read object {oid}: {e}"));
            let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
            let mut decoded = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
            let header_end = decoded.iter().position(|&b| b == 0).unwrap();
            let header = std::str::from_utf8(&decoded[..header_end]).unwrap();
            let body = &decoded[header_end + 1..];
            if header.starts_with("commit ") {
                let body_text = std::str::from_utf8(body).unwrap();
                let tree_line = body_text.lines().next().expect("commit has tree line");
                let tree_oid = tree_line.strip_prefix("tree ").expect("tree prefix");
                to_walk.push((tree_oid.to_string(), "tree"));
            } else if header.starts_with("tree ") {
                // Tree entry: `<mode> <name>\0<20 raw bytes>` (SHA-1).
                let mut cursor = 0;
                while cursor < body.len() {
                    let space_pos = cursor
                        + body[cursor..]
                            .iter()
                            .position(|&b| b == b' ')
                            .expect("mode terminator");
                    let mode = std::str::from_utf8(&body[cursor..space_pos]).unwrap();
                    let name_start = space_pos + 1;
                    let null_pos = name_start
                        + body[name_start..]
                            .iter()
                            .position(|&b| b == 0)
                            .expect("name terminator");
                    let hash_start = null_pos + 1;
                    let hash_bytes = &body[hash_start..hash_start + 20];
                    let child_oid = hex::encode(hash_bytes);
                    let child_type = if mode == "40000" { "tree" } else { "blob" };
                    to_walk.push((child_oid, child_type));
                    cursor = hash_start + 20;
                }
            }
        }
    }

    async fn assert_object_index_has(conn: &DatabaseConnection, oid: &str, expected_o_type: &str) {
        let backend = conn.get_database_backend();
        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT o_type FROM object_index WHERE o_id = ? LIMIT 1",
                [oid.into()],
            ))
            .await
            .unwrap_or_else(|e| panic!("query object_index for {oid}: {e}"))
            .unwrap_or_else(|| panic!("object {oid} missing from object_index"));
        let actual: String = row.try_get_by("o_type").unwrap();
        // A blob may be tagged with a more-specific agent o_type
        // (`agent_transcript`); accept that as a valid upgrade.
        let acceptable = expected_o_type == actual
            || (expected_o_type == "blob" && actual.starts_with("agent_"));
        assert!(
            acceptable,
            "object {oid} has o_type '{actual}', expected '{expected_o_type}' (or agent_* upgrade)"
        );
    }
}
