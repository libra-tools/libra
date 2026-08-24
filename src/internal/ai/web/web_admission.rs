//! Persist-before-gate turn admission shared by the production Code UI adapter.
//!
//! Web-only launches keep worker spawn, approval listeners, and shutdown on a
//! thin headless lifecycle host. Command admission (submit/cancel/respond
//! durability + transcript upsert) lives here so
//! [`super::agent_runtime_adapter::AgentRuntimeCodeUiAdapter`] owns the
//! browser write path.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::anyhow;
use chrono::Utc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    code_ui::{
        CodeUiApiError, CodeUiInteractionKind, CodeUiInteractionResponse, CodeUiInteractionStatus,
        CodeUiSession, CodeUiSessionStatus, CodeUiTranscriptEntry, CodeUiTranscriptEntryKind,
    },
    headless::{
        ConfirmedIntentForPhase1, HeadlessPhase1Command, HeadlessSessionPersistence,
        PendingIntentRevision, prepare_claiming_intent_revision,
        promote_claiming_intent_revision_after_admission, rearm_cancelled_intent_revision_consumer,
        rearm_unadmitted_claiming_intent_revision,
    },
};
use crate::internal::ai::{
    runtime::{
        AgentRuntimeHandle, RuntimeWorkerError, TurnRequest, runtime_worker_adapter_message,
    },
    session::IntentRevisionConsumptionClaim,
};

/// Durable command kind recorded for web-only AgentRuntime turns.
///
/// Plan vs explicit-direct routing is an admission/mode concern
/// ([`WebTurnMode`]), not a separate durability identity. Keep the historical
/// `headless_direct_turn` kind so default Web `--resume` retries of a prior
/// `commandId` still match the stored `CodeCommandIntent` (idempotent ACK).
pub const CODE_UI_WEB_TURN_KIND: &str =
    crate::internal::ai::session::INTENT_REVISION_CONSUMER_COMMAND_KIND;

pub(crate) fn durable_web_turn_intent(
    persistence: &HeadlessSessionPersistence,
    runtime_turn_id: &str,
    input: &str,
) -> crate::internal::ai::session::CodeCommandIntent {
    use sha2::Digest as _;

    let (_, repo_id, principal_id) = persistence.worker_durability_config();
    crate::internal::ai::session::CodeCommandIntent::new(
        crate::internal::ai::session::CodeCommandIdentity::new(
            repo_id,
            persistence.durability_session_id(),
            principal_id,
            runtime_turn_id,
        ),
        CODE_UI_WEB_TURN_KIND,
        format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(input.as_bytes()))
        ),
        true,
    )
}

/// A turn is durable-admitted before the executor may run tools. This compact
/// state lets cancellation win while the executor is waiting on admission's
/// mutex without making graceful shutdown wait for slow session storage.
pub(crate) const PRE_START_UNSTARTED: u8 = 0;
pub(crate) const PRE_START_CANCELLED: u8 = 1;
pub(crate) const PRE_START_STARTED: u8 = 2;
pub(crate) const PHASE1_ATTEMPT_PLANNING: u8 = 0;
pub(crate) const PHASE1_ATTEMPT_MUTATING: u8 = 1;
pub(crate) const PHASE1_ATTEMPT_CANCELLED: u8 = 2;
/// The coordinator has claimed the pre-provider handoff and guarantees that
/// any durable command intent it admits will receive a terminal result.
pub(crate) const PHASE1_ATTEMPT_ADMITTING: u8 = 3;
/// Web Cancel durably persisted the terminal projection and consumed the seed.
/// A detached coordinator settlement may now release owners only; it must not
/// perform another fallible durable close-out and fence an acknowledged cancel.
pub(crate) const PHASE1_ATTEMPT_SETTLED: u8 = 4;

pub(crate) type PreStartTurn = Arc<Mutex<Option<(String, Arc<AtomicU8>)>>>;

/// How a browser message is admitted into the worker executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebTurnMode {
    /// Plain chat → Phase 0 plan tool loop (`phase0_plan_tool_loop_config`).
    PlanPhase0,
    /// Canonical `/intent modify <changes>` → the active IntentSpec
    /// revision consumer, with only `<changes>` sent to the revision prompt.
    IntentRevisionModify,
    /// Canonical `/intent cancel` → durably abandon the active IntentSpec
    /// revision without invoking a completion provider or tool.
    IntentRevisionCancel,
    /// Explicit direct tool loop (slash/`/`-prefixed or other opt-in).
    ExplicitDirect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntentRevisionControl<'a> {
    Modify(&'a str),
    Cancel,
}

fn intent_revision_control_for_message(text: &str) -> Option<IntentRevisionControl<'_>> {
    let trimmed = text.trim();
    if text == crate::internal::ai::session::jsonl::INTENT_REVISION_CANCEL_COMMAND_INPUT {
        return Some(IntentRevisionControl::Cancel);
    }
    let changes = trimmed.strip_prefix("/intent modify")?;
    if changes.is_empty() || !changes.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let changes = changes.trim();
    (!changes.is_empty()).then_some(IntentRevisionControl::Modify(changes))
}

pub(crate) fn intent_revision_modify_note(text: &str) -> Option<&str> {
    match intent_revision_control_for_message(text) {
        Some(IntentRevisionControl::Modify(changes)) => Some(changes),
        Some(IntentRevisionControl::Cancel) | None => None,
    }
}

pub(crate) fn consumes_intent_revision(mode: WebTurnMode) -> bool {
    matches!(
        mode,
        WebTurnMode::PlanPhase0
            | WebTurnMode::IntentRevisionModify
            | WebTurnMode::IntentRevisionCancel
    )
}

/// Web plain-message routing: non-empty text that does not start
/// with `/` enters the plan workflow instead of a mutating direct chat turn.
pub fn should_route_plain_message_to_plan(text: &str) -> bool {
    let trimmed = text.trim_start();
    !trimmed.trim().is_empty() && !trimmed.starts_with('/')
}

pub(crate) fn web_turn_mode_for_message(text: &str) -> WebTurnMode {
    if should_route_plain_message_to_plan(text) {
        WebTurnMode::PlanPhase0
    } else {
        WebTurnMode::ExplicitDirect
    }
}

fn canonical_intent_revision_note_for_web(
    response: &CodeUiInteractionResponse,
) -> anyhow::Result<Option<String>> {
    match super::headless::canonical_intent_revision_note(response) {
        Ok(note) => Ok(note),
        Err(RuntimeWorkerError::InvalidInteractionResponse(message)) => {
            Err(CodeUiApiError::bad_request("INVALID_QUERY_PARAM", message).into())
        }
        Err(error) => Err(anyhow!(
            "failed to validate the IntentSpec Modify note: {}",
            runtime_worker_adapter_message(error)
        )),
    }
}

/// Bookkeeping for a browser turn accepted by the serialized worker.
///
/// The gate keeps the worker executor from running tools before the durable
/// initial projection has been written.
pub(crate) struct InFlightTurn {
    pub(crate) runtime_turn_id: String,
    /// Canonical browser text admitted with this command id (retry compare).
    pub(crate) input: String,
    pub(crate) assistant_entry_id: String,
    pub(crate) mode: WebTurnMode,
    pub(crate) start_gate: Arc<tokio::sync::Notify>,
    pub(crate) start_open: Arc<AtomicBool>,
    /// Signals once terminal UI state and the worker's active-turn slot have
    /// settled, including admission rollback after a durability failure.
    pub(crate) completion: Arc<tokio::sync::Notify>,
}

/// Shared admission state for web-owned Code UI command writes.
pub struct WebCodeUiAdmission {
    pub(crate) in_flight: Arc<Mutex<Option<InFlightTurn>>>,
    /// Bounded one-turn handoff used only while admission has not opened the
    /// executor start gate. A new admission overwrites the prior terminal
    /// entry, so it cannot grow with session history.
    pub(crate) pre_start_turn: PreStartTurn,
    pub(crate) admitted_command_inputs: Arc<Mutex<std::collections::HashMap<String, String>>>,
    pub(crate) next_turn_id: Arc<AtomicU64>,
    pub(crate) active_turn_mutations:
        Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    pub(crate) phase1_attempt_states: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicU8>>>>,
    /// PlanPhase0 IntentSpec review parks keyed by runtime turn id. Shared
    /// with the headless executor so cancel can drop stale entries without
    /// waiting for a respond settle that never runs.
    pub(crate) pending_intent_reviews: Arc<Mutex<std::collections::HashMap<String, String>>>,
    pub(crate) pending_intent_revision: Arc<Mutex<Option<PendingIntentRevision>>>,
    pub(crate) shutting_down: Arc<AtomicBool>,
    /// Shared fail-closed flag used by the executor and Web close-out paths.
    /// A failed fence/marker/snapshot write must prevent later terminal work
    /// from projecting the session as recoverable.
    pub(crate) interaction_persistence_failed: Arc<AtomicBool>,
    pub(crate) persistence: Option<HeadlessSessionPersistence>,
    pub(crate) runtime_session_id: String,
    pub(crate) working_dir: std::path::PathBuf,
    /// Command-only port into the runtime-owned Phase 1 coordinator. Durable
    /// workflow JSONL/context remains authoritative; this channel carries no
    /// adapter-local plan cursor or pending-plan state.
    pub(crate) phase1_tx: tokio::sync::mpsc::Sender<HeadlessPhase1Command>,
    /// Serializes the full prepare/respond/park transition for one runtime
    /// interaction so retries cannot revoke another request's durable gate.
    pub(crate) interaction_transition: Arc<Mutex<()>>,
}

pub(crate) struct WebCodeUiAdmissionInit {
    pub(crate) runtime_session_id: String,
    pub(crate) persistence: Option<HeadlessSessionPersistence>,
    pub(crate) in_flight: Arc<Mutex<Option<InFlightTurn>>>,
    pub(crate) pre_start_turn: PreStartTurn,
    pub(crate) active_turn_mutations:
        Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    pub(crate) phase1_attempt_states: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicU8>>>>,
    pub(crate) interaction_transition: Arc<Mutex<()>>,
    pub(crate) pending_intent_reviews: Arc<Mutex<std::collections::HashMap<String, String>>>,
    pub(crate) pending_intent_revision: Arc<Mutex<Option<PendingIntentRevision>>>,
    pub(crate) shutting_down: Arc<AtomicBool>,
    pub(crate) interaction_persistence_failed: Arc<AtomicBool>,
    pub(crate) phase1_tx: tokio::sync::mpsc::Sender<HeadlessPhase1Command>,
    pub(crate) working_dir: std::path::PathBuf,
}

impl WebCodeUiAdmission {
    pub(crate) fn new(init: WebCodeUiAdmissionInit) -> Arc<Self> {
        let WebCodeUiAdmissionInit {
            runtime_session_id,
            persistence,
            in_flight,
            pre_start_turn,
            active_turn_mutations,
            phase1_attempt_states,
            interaction_transition,
            pending_intent_reviews,
            pending_intent_revision,
            shutting_down,
            interaction_persistence_failed,
            phase1_tx,
            working_dir,
        } = init;
        Arc::new(Self {
            in_flight,
            pre_start_turn,
            admitted_command_inputs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            next_turn_id: Arc::new(AtomicU64::new(1)),
            active_turn_mutations,
            phase1_attempt_states,
            pending_intent_reviews,
            pending_intent_revision,
            shutting_down,
            interaction_persistence_failed,
            persistence,
            runtime_session_id,
            working_dir,
            phase1_tx,
            interaction_transition,
        })
    }

    async fn send_phase1_command(&self, command: HeadlessPhase1Command) -> anyhow::Result<()> {
        const PHASE1_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
        tokio::time::timeout(PHASE1_COMMAND_TIMEOUT, self.phase1_tx.send(command))
            .await
            .map_err(|_| anyhow!("Phase 1 coordinator command queue remained full for 5 seconds"))?
            .map_err(|_| anyhow!("Phase 1 coordinator stopped before accepting the command"))
    }

    pub(crate) fn ensure_not_shutting_down(&self) -> anyhow::Result<()> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(anyhow!(
                "This headless web runtime is shutting down and cannot accept new commands"
            ));
        }
        Ok(())
    }

    pub(crate) async fn ensure_session_is_recoverable(
        &self,
        session: &CodeUiSession,
    ) -> anyhow::Result<()> {
        if session.snapshot().await.status == CodeUiSessionStatus::IndeterminateSideEffect {
            return Err(CodeUiApiError::reconciliation_required(
                "this headless web session has an indeterminate persistence state; restart and inspect its durable session data before sending another request",
            )
            .into());
        }
        Ok(())
    }

    async fn fence_consumed_phase1_response(
        &self,
        runtime: &AgentRuntimeHandle,
        session: &Arc<CodeUiSession>,
        runtime_turn_id: &str,
        _error: &anyhow::Error,
    ) -> anyhow::Error {
        // Durable/UI diagnostics deliberately use a fixed bounded message.
        // The originating error may contain provider text, secrets, or
        // checkout-local paths and must not be copied into JSONL/SSE.
        let reason = "Phase 1 Web close-out is indeterminate; restart and reconcile the durable session before retrying".to_string();
        let mut failed_stages = Vec::new();
        if runtime
            .fence_session(self.runtime_session_id.clone(), reason.clone())
            .await
            .is_err()
        {
            failed_stages.push("runtime fence");
        }
        session
            .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
            .await;
        if let Some(persistence) = self.persistence.as_ref() {
            if persistence
                .goal_event_store()
                .append_code_workflow_durable(
                    crate::internal::ai::session::CodeWorkflowEventKind::IndeterminateSideEffect {
                        command_id: runtime_turn_id.to_string(),
                        effect: "phase1_web_closeout".to_string(),
                        reason: reason.clone(),
                    },
                )
                .is_err()
            {
                failed_stages.push("workflow fence marker");
            }
            if persistence
                .persist_snapshot(session.snapshot().await)
                .await
                .is_err()
            {
                failed_stages.push("session snapshot");
            }
        }
        release_web_turn(&self.in_flight, runtime_turn_id).await;
        self.active_turn_mutations
            .lock()
            .await
            .remove(runtime_turn_id);
        self.phase1_attempt_states
            .lock()
            .await
            .remove(runtime_turn_id);
        if failed_stages.is_empty() {
            anyhow!(reason)
        } else {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            anyhow!(
                "{reason}; durable fence failed at: {}",
                failed_stages.join(", ")
            )
        }
    }

    async fn start_pending_plan_revision(
        &self,
        runtime: &AgentRuntimeHandle,
        session: &Arc<CodeUiSession>,
        runtime_turn_id: &str,
        revision_note: &str,
        browser_command_id: Option<&str>,
    ) -> anyhow::Result<bool> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(false);
        };
        let store = persistence.goal_event_store();
        let replay = store.load_code_workflow_replay()?;
        let Some(interaction_id) =
            crate::internal::ai::runtime::phase1::pending_plan_revision_from_workflow(
                replay.events.iter().map(|event| &event.event),
            )
        else {
            return Ok(false);
        };
        let context_id = crate::internal::ai::runtime::phase1::phase1_context_id_for_interaction(
            replay.events.iter().map(|event| &event.event),
            &interaction_id,
        )
        .ok_or_else(|| anyhow!("pending Plan revision has no durable context binding"))?;
        let context =
            crate::internal::ai::runtime::phase1::load_phase1_review_context(&store, &context_id)?;
        let note = revision_note.trim();
        if note.is_empty() {
            return Err(CodeUiApiError::bad_request(
                "PLAN_REVISION_NOTE_REQUIRED",
                "the next plain message must describe the requested Plan changes",
            )
            .into());
        }
        let checkout =
            crate::internal::ai::runtime::phase1::Phase1CheckoutBinding::capture(
                &self.working_dir,
                &context.intent_spec,
            )
            .await
            .map_err(|error| {
                CodeUiApiError::conflict(
                    "PHASE1_WORKSPACE_CHANGED",
                    format!(
                        "Libra could not capture the current checkout for the Plan revision ({error}); the revision note was not consumed. Retry after filesystem activity settles, or Cancel."
                    ),
                )
            })?;
        if !checkout.same_intent_repository_as(&context.checkout) {
            return Err(CodeUiApiError::conflict(
                "PHASE1_WORKSPACE_CHANGED",
                "The repository identity changed after Modify was selected. The revision note was not consumed; restore the original repository or start a fresh libra code session in the current repository so it receives a new IntentSpec review.",
            )
            .into());
        }
        let prior_plan_id = context.plan_id().map(str::to_string);
        let mut confirmed = ConfirmedIntentForPhase1 {
            source_interaction_id: interaction_id.clone(),
            seed_digest: String::new(),
            intent_id: context.intent_id.clone(),
            intent_spec_id: context.intent_spec_id.clone(),
            intent_spec_json: serde_json::to_string_pretty(&context.intent_spec)?,
            // Preserve the caller's exact text for the public commandId
            // payload contract. Trimming above is validation only.
            revision_note: Some(revision_note.to_string()),
            checkout: Some(checkout.clone()),
            revision_source_interaction_id: Some(interaction_id.clone()),
            prior_plan: Some(context.execution_plan.clone()),
            prior_plan_id: prior_plan_id.clone(),
            prior_persisted_plan: context.persisted_plan.clone(),
            phase1_turn_id_override: browser_command_id.map(str::to_string),
        };
        let completion = Arc::new(tokio::sync::Notify::new());
        {
            let mut slot = self.in_flight.lock().await;
            if let Some(existing) = slot.as_ref() {
                if existing.runtime_turn_id == runtime_turn_id && existing.input == revision_note {
                    return Ok(true);
                }
                if existing.runtime_turn_id == runtime_turn_id {
                    return Err(RuntimeWorkerError::CommandPayloadConflict {
                        session_id: self.runtime_session_id.clone(),
                        turn_id: runtime_turn_id.to_string(),
                    }
                    .into());
                }
                return Err(CodeUiApiError::conflict(
                    "SESSION_BUSY",
                    "A turn is already running; wait before sending the Plan revision note",
                )
                .into());
            }
            *slot = Some(InFlightTurn {
                runtime_turn_id: runtime_turn_id.to_string(),
                input: revision_note.to_string(),
                assistant_entry_id: format!("assistant-plan-revision-{}", Uuid::new_v4()),
                mode: WebTurnMode::PlanPhase0,
                start_gate: Arc::new(tokio::sync::Notify::new()),
                start_open: Arc::new(AtomicBool::new(true)),
                completion,
            });
        }
        let seed = crate::internal::ai::runtime::phase1::Phase1StartSeed {
            schema_version: crate::internal::ai::runtime::phase1::Phase1StartSeed::SCHEMA_VERSION,
            // This Web turn is already serialized and unique. Reusing it here
            // makes a crash retry recover the same durable command, while a
            // new revision submission gets a fresh server turn id.
            attempt_id: runtime_turn_id.to_string(),
            source_interaction_id: interaction_id,
            intent_id: confirmed.intent_id.clone(),
            intent_spec_id: confirmed.intent_spec_id.clone(),
            intent_spec_json: confirmed.intent_spec_json.clone(),
            source_resolution: "modify".to_string(),
            revision_note: confirmed.revision_note.clone(),
            checkout,
            prior_plan: Some(context.execution_plan),
            prior_plan_id,
            prior_persisted_plan: context.persisted_plan,
            browser_command_id: browser_command_id.map(str::to_string),
        };
        confirmed.phase1_turn_id_override =
            Some(crate::internal::ai::runtime::phase1::phase1_turn_id_from_seed(&seed)?);
        confirmed.seed_digest = seed.durable_digest()?;
        if let Err(error) =
            crate::internal::ai::runtime::phase1::persist_phase1_start_seed_idempotent(
                &store, &seed,
            )
        {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                let reconciliation = anyhow!(
                    "Plan revision cannot replace a different durable Phase 1 start seed ({error}); session requires reconciliation"
                );
                return Err(self
                    .fence_consumed_phase1_response(
                        runtime,
                        session,
                        runtime_turn_id,
                        &reconciliation,
                    )
                    .await);
            }
            if let Err(cleanup_error) =
                crate::internal::ai::runtime::phase1::clear_phase1_start_seed(&store)
            {
                let reconciliation = anyhow!(
                    "Plan revision seed persistence failed ({error}) and its possibly-renamed durable seed could not be revoked ({cleanup_error}); session requires reconciliation"
                );
                return Err(self
                    .fence_consumed_phase1_response(
                        runtime,
                        session,
                        runtime_turn_id,
                        &reconciliation,
                    )
                    .await);
            }
            release_web_turn(&self.in_flight, runtime_turn_id).await;
            return Err(anyhow!(
                "Unable to persist the Plan revision start seed; no revision was started: {error}"
            ));
        }

        // Let Runtime/CodeCommandStore decide Execute vs Existing before a
        // transcript row is persisted. Existing successful browser commands
        // must be pure idempotent acknowledgements, not duplicate user rows or
        // a newly orphaned start seed.
        let (admitted_tx, admitted_rx) = tokio::sync::oneshot::channel();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        if self
            .send_phase1_command(HeadlessPhase1Command::Start {
                confirmed,
                admitted: Some(admitted_tx),
                start: Some(start_rx),
            })
            .await
            .is_err()
        {
            let error = anyhow!(
                "Phase 1 coordinator stopped after the durable Plan revision seed was accepted; restart to resume"
            );
            return Err(self
                .fence_consumed_phase1_response(runtime, session, runtime_turn_id, &error)
                .await);
        }
        let admission = admitted_rx
            .await
            .map_err(|_| anyhow!("Phase 1 coordinator stopped before revision admission"))?
            .map_err(|error| anyhow!(error))?;
        if admission == super::headless::Phase1StartAdmission::Existing {
            release_web_turn(&self.in_flight, runtime_turn_id).await;
            return Ok(true);
        }

        let now = Utc::now();
        let user_entry = CodeUiTranscriptEntry {
            id: format!("user-plan-revision-{}", Uuid::new_v4()),
            kind: CodeUiTranscriptEntryKind::UserMessage,
            title: None,
            content: Some(revision_note.to_string()),
            status: Some("submitted".to_string()),
            streaming: false,
            metadata: serde_json::json!({ "webTurnMode": "PlanRevision" }),
            created_at: now,
            updated_at: now,
        };
        let mut durable_snapshot = session.snapshot().await;
        durable_snapshot.transcript.push(user_entry.clone());
        durable_snapshot.status = CodeUiSessionStatus::Thinking;
        durable_snapshot.updated_at = now;
        if let Err(error) = persistence
            .record_user_message(durable_snapshot, revision_note)
            .await
        {
            if start_tx
                .send(Err(format!(
                    "Plan revision transcript persistence failed before provider start: {error}"
                )))
                .is_err()
            {
                let reconciliation = anyhow!(
                    "Plan revision note persistence failed ({error}) after Runtime admission, and the Phase 1 coordinator could not be told to abort; session requires reconciliation"
                );
                return Err(self
                    .fence_consumed_phase1_response(
                        runtime,
                        session,
                        runtime_turn_id,
                        &reconciliation,
                    )
                    .await);
            }
            return Err(anyhow!(
                "Unable to persist the Plan revision note; no revision was started: {error}"
            ));
        }
        session.upsert_transcript_entry(user_entry).await;
        session.set_status(CodeUiSessionStatus::Thinking).await;
        if start_tx.send(Ok(())).is_err() {
            let error = anyhow!(
                "Phase 1 coordinator stopped after Runtime admitted the durable Plan revision note; session requires reconciliation"
            );
            return Err(self
                .fence_consumed_phase1_response(runtime, session, runtime_turn_id, &error)
                .await);
        }
        Ok(true)
    }

    pub(crate) async fn submit_message_with_command_id(
        &self,
        runtime: &AgentRuntimeHandle,
        session: &Arc<CodeUiSession>,
        text: String,
        command_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.ensure_not_shutting_down()?;
        self.ensure_session_is_recoverable(session).await?;
        if command_id.is_some() && self.persistence.is_none() {
            return Err(anyhow!(
                "commandId requires a resumable headless session with durable command storage; omit commandId or enable session persistence"
            ));
        }

        let browser_command_id_supplied = command_id.is_some();
        let runtime_turn_id = match command_id {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(anyhow!(
                        "commandId must be a non-empty string when provided"
                    ));
                }
                if trimmed.chars().count() > 512 {
                    return Err(anyhow!(
                        "commandId must be at most 512 characters (got {})",
                        trimmed.chars().count()
                    ));
                }
                if trimmed
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
                {
                    return Err(anyhow!(
                        "commandId must not contain whitespace or control characters"
                    ));
                }
                trimmed.to_string()
            }
            None => {
                let turn_id = self.next_turn_id.fetch_add(1, Ordering::Relaxed);
                format!("web-{turn_id}-{}", Uuid::new_v4())
            }
        };

        let empty_message = text.trim().is_empty();
        // A blank message is normally invalid, but while Plan Modify authority
        // is pending it must reach the typed PLAN_REVISION_NOTE_REQUIRED guard
        // without being admitted as a direct turn.
        let intent_control = intent_revision_control_for_message(&text);
        let mode = if empty_message {
            WebTurnMode::PlanPhase0
        } else {
            match intent_control {
                Some(IntentRevisionControl::Modify(_)) => WebTurnMode::IntentRevisionModify,
                Some(IntentRevisionControl::Cancel) => WebTurnMode::IntentRevisionCancel,
                None => web_turn_mode_for_message(&text),
            }
        };
        // Serialize every routing decision through durable revision
        // promotion and consumer admission. ExplicitDirect must not sneak a
        // generic Web intent past an Active IntentSpec or Plan revision, and
        // a PlanPhase0 consumer keeps this guard until Claiming has advanced
        // to the event-bound Consuming state.
        let plan_transition = self.interaction_transition.lock().await;
        if browser_command_id_supplied && let Some(persistence) = self.persistence.as_ref() {
            let expected = durable_web_turn_intent(persistence, &runtime_turn_id, &text);
            if let Some((actual, status)) = persistence
                .goal_event_store()
                .code_command_intent_status(&expected.identity)?
            {
                if actual != expected {
                    return Err(RuntimeWorkerError::CommandPayloadConflict {
                        session_id: self.runtime_session_id.clone(),
                        turn_id: runtime_turn_id,
                    }
                    .into());
                }
                match status {
                    crate::internal::ai::session::CodeCommandStatus::Succeeded { .. } => {
                        return Ok(());
                    }
                    crate::internal::ai::session::CodeCommandStatus::Failed { .. } => {
                        return Err(CodeUiApiError::conflict(
                            "COMMAND_ALREADY_TERMINAL",
                            format!(
                                "commandId '{runtime_turn_id}' already finished with state failed; allocate a new commandId to retry"
                            ),
                        )
                        .into());
                    }
                    crate::internal::ai::session::CodeCommandStatus::Indeterminate { .. } => {
                        return Err(RuntimeWorkerError::ReconciliationRequired {
                            session_id: self.runtime_session_id.clone(),
                        }
                        .into());
                    }
                    crate::internal::ai::session::CodeCommandStatus::Pending => {
                        let is_matching_live_retry =
                            self.in_flight.lock().await.as_ref().is_some_and(|turn| {
                                turn.runtime_turn_id == runtime_turn_id && turn.input == text
                            });
                        if !is_matching_live_retry {
                            return Err(RuntimeWorkerError::ReconciliationRequired {
                                session_id: self.runtime_session_id.clone(),
                            }
                            .into());
                        }
                        return Ok(());
                    }
                }
            }
        }

        if let Ok(snapshot) = runtime.snapshot(self.runtime_session_id.clone()).await {
            use crate::internal::ai::runtime::InteractionState;
            if matches!(
                snapshot.interaction,
                InteractionState::Queued
                    | InteractionState::Running
                    | InteractionState::AwaitingIntentReview { .. }
                    | InteractionState::AwaitingPlanReview { .. }
                    | InteractionState::AwaitingPlanRepair { .. }
                    | InteractionState::AwaitingNetworkPolicy { .. }
                    | InteractionState::AwaitingToolApproval { .. }
                    | InteractionState::AwaitingUserInput { .. }
                    | InteractionState::Cancelling
            ) {
                return Err(CodeUiApiError::conflict(
                    "SESSION_BUSY",
                    "A turn is already running; cancel it or wait for the assistant to finish before sending another message",
                )
                .into());
            }
        }

        if let Some(IntentRevisionControl::Modify(changes)) = intent_control
            && changes.len() > crate::internal::ai::session::MAX_INTENT_REVISION_NOTE_BYTES
        {
            return Err(CodeUiApiError::bad_request(
                "INVALID_QUERY_PARAM",
                format!(
                    "IntentSpec Modify note exceeds the {}-byte UTF-8 limit",
                    crate::internal::ai::session::MAX_INTENT_REVISION_NOTE_BYTES
                ),
            )
            .into());
        }
        if mode == WebTurnMode::PlanPhase0
            && self.pending_intent_revision.lock().await.is_some()
            && text.trim().len() > crate::internal::ai::session::MAX_INTENT_REVISION_NOTE_BYTES
        {
            return Err(CodeUiApiError::bad_request(
                "INVALID_QUERY_PARAM",
                format!(
                    "IntentSpec Modify note exceeds the {}-byte UTF-8 limit",
                    crate::internal::ai::session::MAX_INTENT_REVISION_NOTE_BYTES
                ),
            )
            .into());
        }

        // Exact terminal/live retries above are acknowledgements, not new
        // direct execution, so preserve their command-id idempotency contract.
        // Only a command that would append a fresh durable intent is blocked
        // by an outstanding revision gate.
        if matches!(
            mode,
            WebTurnMode::ExplicitDirect
                | WebTurnMode::IntentRevisionModify
                | WebTurnMode::IntentRevisionCancel
        ) {
            let has_active_intent_revision = self.pending_intent_revision.lock().await.is_some();
            let has_pending_plan_revision = if let Some(persistence) = self.persistence.as_ref() {
                let replay = persistence
                    .goal_event_store()
                    .load_code_workflow_replay_committed()?;
                crate::internal::ai::runtime::phase1::pending_plan_revision_from_workflow(
                    replay.events.iter().map(|event| &event.event),
                )
                .is_some()
            } else {
                false
            };
            match mode {
                WebTurnMode::IntentRevisionModify if !has_active_intent_revision => {
                    return Err(CodeUiApiError::conflict(
                        "INTERACTION_NOT_ACTIVE",
                        "`/intent modify <changes>` requires an active IntentSpec revision; start Modify from an IntentSpec review first",
                    )
                    .into());
                }
                WebTurnMode::IntentRevisionCancel if !has_active_intent_revision => {
                    let message = if has_pending_plan_revision {
                        "`/intent cancel` only exits IntentSpec revision mode; this session is waiting for a plain-text Plan revision note"
                    } else {
                        "`/intent cancel` requires an active IntentSpec revision"
                    };
                    return Err(CodeUiApiError::conflict("INTERACTION_NOT_ACTIVE", message).into());
                }
                WebTurnMode::ExplicitDirect if has_active_intent_revision => {
                    return Err(CodeUiApiError::conflict(
                        "SESSION_BUSY",
                        "An IntentSpec revision is waiting; submit a plain-text revision note, use `/intent modify <changes>`, or use `/intent cancel` before running an explicit direct command",
                    )
                    .into());
                }
                WebTurnMode::ExplicitDirect if has_pending_plan_revision => {
                    return Err(CodeUiApiError::conflict(
                        "SESSION_BUSY",
                        "A Plan revision is waiting for the next plain-text message; submit that revision note before running an explicit direct command",
                    )
                    .into());
                }
                _ => {}
            }
        }

        let started_plan_revision = if mode == WebTurnMode::PlanPhase0 {
            // Shutdown sets the admission fence before waiting for this lock.
            // Recheck inside the serialized transition so a submit that passed
            // the outer check cannot start after shutdown scanned attempts.
            self.ensure_not_shutting_down()?;
            self.start_pending_plan_revision(
                runtime,
                session,
                &runtime_turn_id,
                &text,
                browser_command_id_supplied.then_some(runtime_turn_id.as_str()),
            )
            .await?
        } else {
            false
        };
        if started_plan_revision {
            return Ok(());
        }
        if empty_message {
            return Err(anyhow!("Empty messages are not accepted by libra code"));
        }

        let mut slot = self.in_flight.lock().await;
        self.ensure_not_shutting_down()?;
        if let Some(existing) = slot.as_ref() {
            if existing.runtime_turn_id == runtime_turn_id {
                if existing.input == text {
                    return Ok(());
                }
                return Err(RuntimeWorkerError::CommandPayloadConflict {
                    session_id: self.runtime_session_id.clone(),
                    turn_id: runtime_turn_id,
                }
                .into());
            }
            // W5-02: typed 409 SESSION_BUSY (UI-neutral successor to the
            // removed TUI bridge's error) — a bare anyhow here fell through
            // to the 422 UNSUPPORTED_OPERATION fallback and broke the
            // state-matrix wire contract.
            return Err(super::code_ui::CodeUiApiError::conflict(
                "SESSION_BUSY",
                "A turn is already running; cancel it or wait for the assistant to finish before sending another message",
            )
            .into());
        }

        let claiming_revision = if consumes_intent_revision(mode) {
            let pending = self.pending_intent_revision.lock().await.clone();
            match (self.persistence.as_ref(), pending) {
                (Some(persistence), Some(pending)) => {
                    let consumer = durable_web_turn_intent(persistence, &runtime_turn_id, &text);
                    let (pending, claim) =
                        prepare_claiming_intent_revision(persistence, pending, consumer).map_err(
                            |error| {
                                anyhow!(
                                    "Unable to persist the IntentSpec revision consumer claim before Runtime admission; no command was admitted: {error}"
                                )
                            },
                        )?;
                    *self.pending_intent_revision.lock().await = Some(pending.clone());
                    Some((pending, claim))
                }
                _ => None,
            }
        } else {
            None
        };

        let user_entry_id = format!("user-{}", Uuid::new_v4());
        let assistant_entry_id = format!("assistant-{}", Uuid::new_v4());
        let now = Utc::now();
        let user_entry = CodeUiTranscriptEntry {
            id: user_entry_id,
            kind: CodeUiTranscriptEntryKind::UserMessage,
            title: None,
            content: Some(text.clone()),
            status: Some("submitted".to_string()),
            streaming: false,
            metadata: serde_json::json!({ "webTurnMode": format!("{mode:?}") }),
            created_at: now,
            updated_at: now,
        };
        let assistant_entry = CodeUiTranscriptEntry {
            id: assistant_entry_id.clone(),
            kind: CodeUiTranscriptEntryKind::AssistantMessage,
            title: None,
            content: Some(String::new()),
            status: Some("streaming".to_string()),
            streaming: true,
            metadata: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        };
        let start_gate = Arc::new(tokio::sync::Notify::new());
        let start_open = Arc::new(AtomicBool::new(false));
        let pre_start_state = Arc::new(AtomicU8::new(PRE_START_UNSTARTED));
        let completion = Arc::new(tokio::sync::Notify::new());
        let completion_for_rollback = completion.clone();
        *slot = Some(InFlightTurn {
            runtime_turn_id: runtime_turn_id.clone(),
            input: text.clone(),
            assistant_entry_id: assistant_entry_id.clone(),
            mode,
            start_gate: start_gate.clone(),
            start_open: start_open.clone(),
            completion,
        });
        *self.pre_start_turn.lock().await =
            Some((runtime_turn_id.clone(), pre_start_state.clone()));

        let submission = runtime
            .submit(TurnRequest::new(
                self.runtime_session_id.clone(),
                runtime_turn_id.clone(),
                text.clone(),
                true,
            ))
            .await;
        if let Err(error) = submission {
            *slot = None;
            if let Some((pending, claim)) = claiming_revision.as_ref() {
                match self.persistence.as_ref().map(|persistence| {
                    rearm_unadmitted_claiming_intent_revision(persistence, pending, claim)
                }) {
                    Some(Ok(true)) => {}
                    Some(Ok(false)) => {
                        drop(plan_transition);
                        session
                            .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                            .await;
                        return Err(CodeUiApiError::reconciliation_required(
                            "the IntentSpec revision consumer may have reached durable Runtime admission; restart to reconcile it before retrying",
                        )
                        .into());
                    }
                    Some(Err(rearm_error)) => {
                        drop(plan_transition);
                        session
                            .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                            .await;
                        return Err(anyhow!(
                            "Runtime admission failed and the unadmitted IntentSpec revision claim could not be safely rearmed: {rearm_error}"
                        ));
                    }
                    None => {}
                }
            }
            drop(plan_transition);
            match error {
                RuntimeWorkerError::IdempotentCommand { ack_ok: true, .. } => {
                    return Ok(());
                }
                RuntimeWorkerError::DuplicateTurn { turn_id, .. } if turn_id == runtime_turn_id => {
                    let admitted = self
                        .admitted_command_inputs
                        .lock()
                        .await
                        .get(&runtime_turn_id)
                        .cloned();
                    debug_assert!(slot.is_none());
                    match admitted.as_deref() {
                        Some(prior) if prior == text => return Ok(()),
                        _ => {
                            return Err(RuntimeWorkerError::CommandPayloadConflict {
                                session_id: self.runtime_session_id.clone(),
                                turn_id: runtime_turn_id,
                            }
                            .into());
                        }
                    }
                }
                RuntimeWorkerError::IdempotentCommand {
                    ack_ok: false,
                    turn_id,
                    status,
                    ..
                } => {
                    return Err(CodeUiApiError::conflict(
                        "COMMAND_ALREADY_TERMINAL",
                        format!(
                            "commandId '{turn_id}' already finished with state {status}; allocate a new commandId to retry"
                        ),
                    )
                    .into());
                }
                RuntimeWorkerError::CommandPayloadConflict { .. }
                | RuntimeWorkerError::ReconciliationRequired { .. } => {
                    return Err(error.into());
                }
                other => {
                    return Err(anyhow!(
                        "Unable to admit the browser turn to the AgentRuntime queue; no turn was started: {}",
                        runtime_worker_adapter_message(other)
                    ));
                }
            }
        }

        if let Some((pending, claim)) = claiming_revision.as_ref()
            && let Some(persistence) = self.persistence.as_ref()
            && let Err(error) =
                promote_claiming_intent_revision_after_admission(persistence, pending, claim)
        {
            // Runtime has fsynced the exact command, but the executor remains
            // behind the unopened start gate. Cancel and wait for its
            // canonical pre-mutation terminal before rearming Active.
            drop(slot);
            let cancel_result = self
                .cancel_gated_runtime_turn(
                    runtime,
                    &runtime_turn_id,
                    completion_for_rollback.clone(),
                )
                .await;
            let rearm_result = cancel_result
                .as_ref()
                .ok()
                .map(|_| rearm_cancelled_intent_revision_consumer(persistence, pending, claim));
            drop(plan_transition);
            match (cancel_result, rearm_result) {
                (Ok(()), Some(Ok(()))) => {
                    return Err(anyhow!(
                        "Unable to persist the event-bound IntentSpec revision consumer; the gated command was cancelled before mutation and the revision remains available: {error}"
                    ));
                }
                (cancel, rearm) => {
                    session
                        .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                        .await;
                    return Err(anyhow!(
                        "IntentSpec revision consumer admission could not be completed safely ({error}); gated cancellation: {}; revision rearm: {}",
                        cancel
                            .err()
                            .map_or_else(|| "completed".to_string(), |error| error.to_string()),
                        rearm
                            .and_then(Result::err)
                            .map_or_else(|| "not attempted".to_string(), |error| error.to_string())
                    ));
                }
            }
        }
        {
            let mut admitted = self.admitted_command_inputs.lock().await;
            const ADMITTED_COMMAND_INPUT_LIMIT: usize = 64;
            admitted.insert(runtime_turn_id.clone(), text.clone());
            while admitted.len() > ADMITTED_COMMAND_INPUT_LIMIT {
                if let Some(evict) = admitted.keys().next().cloned() {
                    if evict == runtime_turn_id {
                        break;
                    }
                    admitted.remove(&evict);
                } else {
                    break;
                }
            }
        }

        // Release the transition lock before the durable write so shutdown can
        // cancel an unstarted worker turn without waiting on session storage.
        drop(plan_transition);
        if let Some(persistence) = self.persistence.as_ref() {
            let mut durable_snapshot = session.snapshot().await;
            durable_snapshot.transcript.push(user_entry.clone());
            durable_snapshot.transcript.push(assistant_entry.clone());
            durable_snapshot.status = CodeUiSessionStatus::Thinking;
            durable_snapshot.updated_at = now;
            if let Err(error) = persistence
                .record_user_message(durable_snapshot, &text)
                .await
            {
                // The executor needs this slot to observe the closed start
                // gate and release it after cancellation. Do not retain the
                // admission lock while waiting for that completion.
                drop(slot);
                self.cancel_gated_runtime_turn(runtime, &runtime_turn_id, completion_for_rollback)
                    .await?;
                self.rearm_cancelled_revision_if_present(session, claiming_revision.as_ref())
                    .await?;
                return Err(anyhow!(
                    "Unable to persist the headless web message; no turn was started. Verify session storage and retry: {error}"
                ));
            }
        }
        if self.shutting_down.load(Ordering::Acquire)
            || pre_start_state.load(Ordering::Acquire) == PRE_START_CANCELLED
        {
            // Shutdown cancelled the worker while the durable admission write
            // was in flight. Its executor returned without acquiring this
            // lock, so admission owns the one terminal browser/durable
            // projection and must not publish a fresh Thinking turn.
            pre_start_state.store(PRE_START_CANCELLED, Ordering::Release);
            drop(slot);
            self.settle_cancelled_before_start(
                session,
                user_entry,
                assistant_entry,
                &runtime_turn_id,
            )
            .await?;
            self.rearm_cancelled_revision_if_present(session, claiming_revision.as_ref())
                .await?;
            return Ok(());
        }
        session.upsert_transcript_entry(user_entry.clone()).await;
        session
            .upsert_transcript_entry(assistant_entry.clone())
            .await;
        session.set_status(CodeUiSessionStatus::Thinking).await;
        if pre_start_state
            .compare_exchange(
                PRE_START_UNSTARTED,
                PRE_START_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            // Cancellation won while live projections were being appended.
            // Do not open the tool gate; replace the just-published streaming
            // entry with the one terminal no-tool result.
            drop(slot);
            self.settle_cancelled_before_start(
                session,
                user_entry,
                assistant_entry,
                &runtime_turn_id,
            )
            .await?;
            self.rearm_cancelled_revision_if_present(session, claiming_revision.as_ref())
                .await?;
            return Ok(());
        }
        start_open.store(true, Ordering::Release);
        start_gate.notify_waiters();
        // Keep the admission slot locked until the durable and live
        // projections are both visible and the start gate is open. An executor
        // cancelled while waiting for this lock returns promptly; the
        // shutdown branch above then commits the sole terminal projection
        // instead of letting this tail republish Thinking.
        drop(slot);
        Ok(())
    }

    /// Commit a terminal, no-tool cancellation when cancellation wins before
    /// admission opens the executor start gate. The worker cannot safely
    /// write this projection because it was cancelled before obtaining the
    /// admission slot.
    async fn settle_cancelled_before_start(
        &self,
        session: &Arc<CodeUiSession>,
        user_entry: CodeUiTranscriptEntry,
        mut assistant_entry: CodeUiTranscriptEntry,
        runtime_turn_id: &str,
    ) -> anyhow::Result<()> {
        assistant_entry.content = Some("(turn cancelled before execution started)".to_string());
        assistant_entry.status = Some("cancelled".to_string());
        assistant_entry.streaming = false;
        assistant_entry.updated_at = Utc::now();
        session.upsert_transcript_entry(user_entry).await;
        session.upsert_transcript_entry(assistant_entry).await;
        session.set_status(CodeUiSessionStatus::Idle).await;

        if let Some(persistence) = self.persistence.as_ref()
            && let Err(error) = persistence.persist_snapshot(session.snapshot().await).await
        {
            // The durable admission projection is still Thinking if this
            // correction cannot be written. Do not let the in-memory Idle
            // state imply that it is safe to resume: fence the session and
            // make one best-effort durable record of that reconciliation
            // boundary before returning the admission error.
            session
                .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                .await;
            if let Err(reconciliation_error) =
                persistence.persist_snapshot(session.snapshot().await).await
            {
                tracing::error!(
                    error = %reconciliation_error,
                    "failed to persist reconciliation state after shutdown-cancelled web admission"
                );
            }
            release_web_turn(&self.in_flight, runtime_turn_id).await;
            return Err(anyhow!(
                "Unable to persist the shutdown-cancelled headless web turn; the durable session may still show a running turn and requires reconciliation before resuming: {error}"
            ));
        }
        release_web_turn(&self.in_flight, runtime_turn_id).await;
        Ok(())
    }

    pub(crate) async fn respond_interaction(
        self: &Arc<Self>,
        runtime: &AgentRuntimeHandle,
        session: &Arc<CodeUiSession>,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> anyhow::Result<()> {
        let _transition = self.interaction_transition.lock().await;
        self.ensure_not_shutting_down()?;
        self.ensure_session_is_recoverable(session).await?;
        let pending = session
            .snapshot()
            .await
            .interactions
            .into_iter()
            .find(|item| {
                item.id == interaction_id
                    && item.status == super::code_ui::CodeUiInteractionStatus::Pending
            });
        if pending.is_none() {
            if let (Some(persistence), Some(requested)) = (
                self.persistence.as_ref(),
                workflow_resolution_label(&response),
            ) {
                let replay = persistence
                    .goal_event_store()
                    .load_code_workflow_replay_committed()?;
                let gate_kind = workflow_gate_kind_for_interaction(
                    replay.events.iter().map(|event| &event.event),
                    interaction_id,
                );
                let retry_intent_revision_note =
                    if matches!(gate_kind, Some(WorkflowGateKind::Intent))
                        && crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(
                            requested,
                        ) == Some(
                            crate::internal::ai::runtime::phase0::IntentReviewDecision::Revise,
                        )
                    {
                        Some(canonical_intent_revision_note_for_web(&response)?)
                    } else {
                        None
                    };
                let durable = replay.events.iter().rev().find_map(|event| {
                    match &event.event {
                        crate::internal::ai::session::CodeWorkflowEventKind::InteractionResolved {
                            interaction_id: candidate,
                            resolution,
                            intent_revision_consumption: None,
                            ..
                        }
                        | crate::internal::ai::session::CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                            interaction_id: candidate,
                            resolution,
                            ..
                        } if candidate == interaction_id => Some(resolution.as_str()),
                        _ => None,
                    }
                });
                if let Some(durable) = durable {
                    if gate_kind
                        .is_some_and(|kind| workflow_resolutions_match(kind, requested, durable))
                    {
                        if matches!(gate_kind, Some(WorkflowGateKind::Intent))
                            && crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(
                                requested,
                            ) == Some(
                                crate::internal::ai::runtime::phase0::IntentReviewDecision::Revise,
                            )
                        {
                            let note = retry_intent_revision_note.flatten();
                            let persistence = self.persistence.as_ref().ok_or_else(|| {
                                anyhow!("IntentSpec Modify retry requires durable session persistence")
                            })?;
                            match super::headless::verify_resolved_intent_revision_retry(
                                persistence,
                                interaction_id,
                                note,
                                false,
                            ) {
                                Ok(true) => {}
                                Ok(false) => {
                                    return Err(CodeUiApiError::conflict(
                                        "INTERACTION_NOT_ACTIVE",
                                        format!(
                                            "interaction '{interaction_id}' is already resolved with a different Modify note"
                                        ),
                                    )
                                    .into());
                                }
                                Err(error) => {
                                    return Err(self
                                        .fence_consumed_phase1_response(
                                            runtime,
                                            session,
                                            interaction_id,
                                            &error,
                                        )
                                        .await);
                                }
                            }
                        }
                        return Ok(());
                    }
                    return Err(CodeUiApiError::conflict(
                        "INTERACTION_NOT_ACTIVE",
                        format!(
                            "interaction '{interaction_id}' is already resolved as '{durable}', not requested '{requested}'"
                        ),
                    )
                    .into());
                }
            }
            return Err(CodeUiApiError::conflict(
                "INTERACTION_NOT_ACTIVE",
                format!("interaction '{interaction_id}' is not pending"),
            )
            .into());
        }
        let selected_option = response.selected_option.clone();
        let intent_review_decision = pending.as_ref().and_then(|item| {
            (item.kind == CodeUiInteractionKind::IntentReviewChoice)
                .then(|| {
                    selected_option.as_deref().and_then(
                        crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id,
                    )
                })
                .flatten()
        });
        // Reject an oversized Modify note before preparing any follow-up,
        // persisting a Phase 1 seed, or handing the response to AgentRuntime.
        // The still-open gate therefore remains correctable by the caller.
        let intent_revision_note = if intent_review_decision
            == Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Revise)
        {
            canonical_intent_revision_note_for_web(&response)?
        } else {
            None
        };
        let mut confirmed_intent = pending.as_ref().and_then(|item| {
            (item.kind == super::code_ui::CodeUiInteractionKind::IntentReviewChoice
                && intent_review_decision
                    == Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Confirm))
            .then(|| ConfirmedIntentForPhase1 {
                source_interaction_id: interaction_id.to_string(),
                seed_digest: String::new(),
                intent_id: item
                    .metadata
                    .get("intentId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                intent_spec_id: String::new(),
                intent_spec_json: item
                    .metadata
                    .get("intentSpec")
                    .and_then(|value| {
                        value.as_str().map(str::to_owned).or_else(|| {
                            value
                                .is_object()
                                .then(|| serde_json::to_string_pretty(value).ok())
                                .flatten()
                        })
                    })
                    .unwrap_or_default(),
                revision_note: None,
                checkout: None,
                revision_source_interaction_id: None,
                prior_plan: None,
                prior_plan_id: None,
                prior_persisted_plan:
                    crate::internal::ai::runtime::phase1::Phase1PersistedPlan::Unavailable,
                phase1_turn_id_override: None,
            })
        });
        let is_post_plan = pending
            .as_ref()
            .is_some_and(|item| item.kind == super::code_ui::CodeUiInteractionKind::PostPlanChoice);
        let is_network_policy = pending.as_ref().is_some_and(|item| {
            item.metadata
                .get("phase")
                .and_then(serde_json::Value::as_str)
                == Some("networkPolicy")
        });
        let plan_decision = (is_post_plan && !is_network_policy)
            .then(|| {
                selected_option.as_deref().and_then(
                    crate::internal::ai::runtime::phase1::PlanReviewDecision::from_wire_id,
                )
            })
            .flatten();
        let network_decision = is_network_policy
            .then(|| {
                selected_option.as_deref().and_then(
                    crate::internal::ai::runtime::phase1::NetworkPolicyDecision::from_wire_id,
                )
            })
            .flatten();
        let terminal_phase1_context_id = if plan_decision
            == Some(crate::internal::ai::runtime::phase1::PlanReviewDecision::Cancel)
            || network_decision
                == Some(crate::internal::ai::runtime::phase1::NetworkPolicyDecision::Deny)
        {
            if let Some(persistence) = self.persistence.as_ref() {
                let store = persistence.goal_event_store();
                let replay = store.load_code_workflow_replay()?;
                crate::internal::ai::runtime::phase1::phase1_context_id_for_gate_interaction(
                    replay.events.iter().map(|event| &event.event),
                    interaction_id,
                )
            } else {
                None
            }
        } else {
            None
        };
        let network_allow = network_decision
            == Some(crate::internal::ai::runtime::phase1::NetworkPolicyDecision::Allow);
        if plan_decision == Some(crate::internal::ai::runtime::phase1::PlanReviewDecision::Revise) {
            let persistence = self.persistence.as_ref().ok_or_else(|| {
                anyhow!("Plan Modify requires durable Phase 1 session persistence")
            })?;
            let store = persistence.goal_event_store();
            let replay = store.load_code_workflow_replay()?;
            let context_id =
                crate::internal::ai::runtime::phase1::phase1_context_id_for_interaction(
                    replay.events.iter().map(|event| &event.event),
                    interaction_id,
                )
                .ok_or_else(|| anyhow!("Plan Modify has no durable context binding"))?;
            let context = crate::internal::ai::runtime::phase1::load_phase1_review_context(
                &store,
                &context_id,
            )?;
            context
                .checkout
                .validate_same_intent_repository(&self.working_dir, &context.intent_spec)
                .await
                .map_err(|error| {
                    CodeUiApiError::conflict(
                        "PHASE1_WORKSPACE_CHANGED",
                        format!(
                            "The repository identity changed since this Plan was reviewed ({error}); the Plan gate remains pending. Cancel and start a new request so the current repository receives a fresh IntentSpec review."
                        ),
                    )
                })?;
        }
        let plan_execute = is_post_plan
            && !is_network_policy
            && plan_decision
                == Some(crate::internal::ai::runtime::phase1::PlanReviewDecision::Execute);
        let mut prepared_network = None;
        if plan_execute {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            self.send_phase1_command(HeadlessPhase1Command::PrepareNetwork {
                plan_interaction_id: interaction_id.to_string(),
                reply: reply_tx,
            })
            .await?;
            prepared_network = Some(
                reply_rx
                    .await
                    .map_err(|_| anyhow!("Phase 1 coordinator dropped network-gate prepare"))?
                    .map_err(|error| anyhow!(error))?,
            );
        }
        if let Some(confirmed) = confirmed_intent.as_mut() {
            if confirmed.intent_id.trim().is_empty() || confirmed.intent_spec_json.trim().is_empty()
            {
                return Err(anyhow!(
                    "Confirmed IntentSpec is missing durable Phase 1 metadata"
                ));
            }
            let persistence = self.persistence.as_ref().ok_or_else(|| {
                anyhow!("IntentSpec confirm requires durable Phase 1 session persistence")
            })?;
            let spec: crate::internal::ai::intentspec::IntentSpec =
                serde_json::from_str(&confirmed.intent_spec_json).map_err(|error| {
                    anyhow!("confirmed IntentSpec cannot capture checkout binding: {error}")
                })?;
            let store = persistence.goal_event_store();
            if let Some(seed) =
                crate::internal::ai::runtime::phase1::load_phase1_start_seed(&store)?
            {
                let same_spec = serde_json::from_str::<serde_json::Value>(&seed.intent_spec_json)
                    .ok()
                    == serde_json::from_str::<serde_json::Value>(&confirmed.intent_spec_json).ok();
                let compatible = seed.attempt_id == interaction_id
                    && seed.source_interaction_id == interaction_id
                    && seed.source_resolution == "confirm"
                    && seed.intent_id == confirmed.intent_id
                    && seed.intent_spec_id == spec.metadata.id
                    && same_spec
                    && seed.revision_note.is_none()
                    && seed.browser_command_id.is_none();
                if !compatible {
                    let runtime_turn_id = self
                        .in_flight
                        .lock()
                        .await
                        .as_ref()
                        .map(|turn| turn.runtime_turn_id.clone())
                        .unwrap_or_else(|| interaction_id.to_string());
                    let reconciliation = anyhow!(
                        "Intent Confirm conflicts with a different durable Phase 1 start authority"
                    );
                    return Err(self
                        .fence_consumed_phase1_response(
                            runtime,
                            session,
                            &runtime_turn_id,
                            &reconciliation,
                        )
                        .await);
                }
                confirmed.intent_id = seed.intent_id.clone();
                confirmed.intent_spec_id = seed.intent_spec_id.clone();
                confirmed.intent_spec_json = seed.intent_spec_json.clone();
                confirmed.checkout = Some(seed.checkout.clone());
                confirmed.prior_plan = seed.prior_plan.clone();
                confirmed.prior_plan_id = seed.prior_plan_id.clone();
                confirmed.prior_persisted_plan = seed.prior_persisted_plan.clone();
                confirmed.phase1_turn_id_override =
                    Some(crate::internal::ai::runtime::phase1::phase1_turn_id_from_seed(&seed)?);
                confirmed.seed_digest = seed.durable_digest()?;
                // The first request's checkout binding remains authoritative;
                // Phase 1 validation will deterministically rearm a fresh gate
                // if the workspace moved before generation.
            } else {
                let checkout =
                    crate::internal::ai::runtime::phase1::Phase1CheckoutBinding::capture(
                        &self.working_dir,
                        &spec,
                    )
                    .await?;
                confirmed.intent_spec_id = spec.metadata.id.clone();
                let seed = crate::internal::ai::runtime::phase1::Phase1StartSeed {
                    schema_version:
                        crate::internal::ai::runtime::phase1::Phase1StartSeed::SCHEMA_VERSION,
                    // The interaction id is the durable Confirm generation. An
                    // HTTP retry of this generation must reuse its Phase 1
                    // attempt; a determinate failure rearms a fresh interaction
                    // id and therefore a fresh attempt.
                    attempt_id: interaction_id.to_string(),
                    source_interaction_id: interaction_id.to_string(),
                    intent_id: confirmed.intent_id.clone(),
                    intent_spec_id: spec.metadata.id.clone(),
                    intent_spec_json: confirmed.intent_spec_json.clone(),
                    source_resolution: "confirm".to_string(),
                    revision_note: None,
                    checkout: checkout.clone(),
                    prior_plan: None,
                    prior_plan_id: None,
                    prior_persisted_plan:
                        crate::internal::ai::runtime::phase1::Phase1PersistedPlan::Unavailable,
                    browser_command_id: None,
                };
                confirmed.phase1_turn_id_override =
                    Some(crate::internal::ai::runtime::phase1::phase1_turn_id_from_seed(&seed)?);
                confirmed.seed_digest = seed.durable_digest()?;
                if let Err(error) =
                    crate::internal::ai::runtime::phase1::persist_phase1_start_seed_idempotent(
                        &persistence.goal_event_store(),
                        &seed,
                    )
                {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        let runtime_turn_id = self
                            .in_flight
                            .lock()
                            .await
                            .as_ref()
                            .map(|turn| turn.runtime_turn_id.clone())
                            .unwrap_or_else(|| interaction_id.to_string());
                        let reconciliation = anyhow!(
                            "Intent Confirm cannot replace a different durable Phase 1 start seed ({error}); session requires reconciliation"
                        );
                        return Err(self
                            .fence_consumed_phase1_response(
                                runtime,
                                session,
                                &runtime_turn_id,
                                &reconciliation,
                            )
                            .await);
                    }
                    if let Err(cleanup_error) =
                        crate::internal::ai::runtime::phase1::clear_phase1_start_seed(
                            &persistence.goal_event_store(),
                        )
                    {
                        let runtime_turn_id = self
                            .in_flight
                            .lock()
                            .await
                            .as_ref()
                            .map(|turn| turn.runtime_turn_id.clone())
                            .unwrap_or_else(|| interaction_id.to_string());
                        let reconciliation = anyhow!(
                            "Intent Confirm seed persistence failed ({error}) and its possibly-renamed seed could not be revoked ({cleanup_error}); session requires reconciliation"
                        );
                        return Err(self
                            .fence_consumed_phase1_response(
                                runtime,
                                session,
                                &runtime_turn_id,
                                &reconciliation,
                            )
                            .await);
                    }
                    return Err(anyhow!(
                        "Unable to persist the Intent Confirm Phase 1 seed; the Confirm gate remains pending: {error}"
                    ));
                }
                confirmed.checkout = Some(checkout);
            }
        }
        let mut prepared_plan_back = None;
        if network_decision
            == Some(crate::internal::ai::runtime::phase1::NetworkPolicyDecision::Back)
        {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            self.send_phase1_command(HeadlessPhase1Command::PreparePlanBack {
                network_interaction_id: interaction_id.to_string(),
                reply: reply_tx,
            })
            .await?;
            prepared_plan_back = Some(
                reply_rx
                    .await
                    .map_err(|_| anyhow!("Phase 1 coordinator dropped Plan Back prepare"))?
                    .map_err(|error| anyhow!(error))?,
            );
        }
        let runtime_turn_id = {
            let slot = self.in_flight.lock().await;
            slot.as_ref().map(|turn| turn.runtime_turn_id.clone())
        };
        let Some(runtime_turn_id) = runtime_turn_id else {
            return Err(anyhow!(CodeUiApiError::conflict(
                "INTERACTION_NOT_ACTIVE",
                format!(
                    "interaction '{interaction_id}' has no active AgentRuntime turn to receive a response"
                )
            )));
        };
        let intent_revision_sidecar_digest = if intent_review_decision
            == Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Revise)
        {
            let persistence = self
                .persistence
                .as_ref()
                .ok_or_else(|| anyhow!("IntentSpec Modify requires durable session persistence"))?;
            match super::headless::prepare_intent_revision_sidecar(
                persistence,
                interaction_id,
                &runtime_turn_id,
                intent_revision_note.clone(),
            ) {
                Ok(digest) => Some(digest),
                Err(RuntimeWorkerError::InvalidInteractionResponse(message)) => {
                    return Err(CodeUiApiError::bad_request("INVALID_QUERY_PARAM", message).into());
                }
                Err(RuntimeWorkerError::InteractionResponseConflict { .. }) => {
                    return Err(CodeUiApiError::conflict(
                        "INTERACTION_NOT_ACTIVE",
                        format!(
                            "interaction '{interaction_id}' already has a different prepared Modify response"
                        ),
                    )
                    .into());
                }
                Err(error) => {
                    let error = anyhow!(
                        "IntentSpec Modify could not establish its durable prepared sidecar: {}",
                        runtime_worker_adapter_message(error)
                    );
                    return Err(self
                        .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                        .await);
                }
            }
        } else {
            None
        };
        let response_payload = if is_post_plan {
            selected_option
                .clone()
                .ok_or_else(|| anyhow!("post_plan_choice requires an explicit selectedOption"))?
        } else {
            let mut canonical_response = response.clone();
            if intent_review_decision
                == Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Revise)
            {
                canonical_response.note = intent_revision_note.clone();
            }
            serde_json::to_string(&canonical_response).map_err(|error| {
                anyhow!(
                    "Unable to encode the interaction response for AgentRuntime delivery: {error}"
                )
            })?
        };
        // Runtime owns the bounded pre-registration response handoff. Web
        // therefore performs one delivery attempt: a true UnknownInteraction
        // is stale/ambiguous and must not be guessed away with timing polls.
        let is_intent_review = session.snapshot().await.interactions.iter().any(|item| {
            item.id == interaction_id
                && matches!(
                    item.kind,
                    super::code_ui::CodeUiInteractionKind::IntentReviewChoice
                )
        });
        let requested_durable_resolution = if is_post_plan {
            selected_option.as_deref()
        } else if is_intent_review {
            intent_review_resolution_label(&response)
        } else {
            None
        };
        let response_gate_kind = if is_network_policy {
            WorkflowGateKind::Network
        } else if is_post_plan {
            WorkflowGateKind::Plan
        } else {
            WorkflowGateKind::Intent
        };
        let mut runtime_response = crate::internal::ai::runtime::InteractionResponse::new(
            interaction_id,
            response_payload.clone(),
        );
        if let Some(digest) = intent_revision_sidecar_digest.as_ref() {
            runtime_response = runtime_response.with_intent_revision_sidecar_digest(digest.clone());
        }
        match runtime
            .respond(
                self.runtime_session_id.clone(),
                runtime_turn_id.clone(),
                runtime_response,
            )
            .await
        {
            Ok(()) => {}
            Err(RuntimeWorkerError::InteractionResponseConflict { .. }) => {
                return Err(CodeUiApiError::conflict(
                        "INTERACTION_NOT_ACTIVE",
                        format!(
                            "interaction '{interaction_id}' is already being answered with a different response"
                        ),
                    )
                    .into());
            }
            Err(RuntimeWorkerError::InvalidInteractionResponse(_)) => {
                return Err(CodeUiApiError::bad_request(
                    "INVALID_QUERY_PARAM",
                    "selectedOption is invalid for this pending interaction",
                )
                .into());
            }
            Err(RuntimeWorkerError::InteractionAlreadyPending { .. }) => {
                return Err(CodeUiApiError::conflict(
                        "INTERACTION_NOT_ACTIVE",
                        format!(
                            "interaction '{interaction_id}' already has the maximum number of response retries waiting for completion"
                        ),
                    )
                    .into());
            }
            Err(error @ RuntimeWorkerError::DurabilityFailure(_))
                if !is_intent_review && !is_post_plan && !is_network_policy =>
            {
                // A non-terminal tool/user-input response is checkpointed
                // before its browser acknowledgement. On checkpoint
                // failure the runtime retains the active executor, cancels
                // it when still pre-mutation, and owns terminal settlement.
                // Fencing here would discard that retained owner and allow
                // a late executor to mutate without a serializing slot.
                return Err(anyhow!(error));
            }
            Err(error) => {
                let may_be_lost_terminal_ack = matches!(
                    &error,
                    RuntimeWorkerError::UnknownTurn { .. }
                        | RuntimeWorkerError::UnknownInteraction { .. }
                        | RuntimeWorkerError::InteractionRegistrationClosed { .. }
                );
                let runtime_is_determinate = if may_be_lost_terminal_ack {
                    runtime
                            .snapshot(self.runtime_session_id.clone())
                            .await
                            .is_ok_and(|snapshot| {
                                !matches!(
                                    snapshot.interaction,
                                    crate::internal::ai::runtime::InteractionState::IndeterminateSideEffect { .. }
                                )
                            })
                } else {
                    false
                };
                if !runtime_is_determinate {
                    let ambiguity = anyhow!(
                        "AgentRuntime response delivery failed without a determinate lost-ack state; session requires reconciliation: {error}"
                    );
                    return Err(self
                        .fence_consumed_phase1_response(
                            runtime,
                            session,
                            &runtime_turn_id,
                            &ambiguity,
                        )
                        .await);
                }
                let durable_resolution = if requested_durable_resolution.is_some() {
                    self.persistence
                            .as_ref()
                            .map(|persistence| {
                                persistence
                                    .goal_event_store()
                                    .load_code_workflow_replay_committed()
                                    .map(|replay| {
                                        replay.events.iter().rev().find_map(|event| {
                                            match &event.event {
                                                crate::internal::ai::session::CodeWorkflowEventKind::InteractionResolved {
                                                    interaction_id: candidate,
                                                    resolution,
                                                    intent_revision_consumption: None,
                                                    ..
                                                }
                                                | crate::internal::ai::session::CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                                                    interaction_id: candidate,
                                                    resolution,
                                                    ..
                                                } if candidate == interaction_id => {
                                                    Some(resolution.clone())
                                                }
                                                _ => None,
                                            }
                                        })
                                    })
                            })
                            .transpose()?
                            .flatten()
                } else {
                    None
                };
                match (requested_durable_resolution, durable_resolution.as_deref()) {
                    (Some(requested), Some(durable))
                        if workflow_resolutions_match(response_gate_kind, requested, durable) =>
                    {
                        if response_gate_kind == WorkflowGateKind::Intent
                                && crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(
                                    requested,
                                ) == Some(
                                    crate::internal::ai::runtime::phase0::IntentReviewDecision::Revise,
                                )
                            {
                                let persistence = self.persistence.as_ref().ok_or_else(|| {
                                    anyhow!("IntentSpec Modify retry requires durable session persistence")
                                })?;
                                match super::headless::verify_resolved_intent_revision_retry(
                                    persistence,
                                    interaction_id,
                                    intent_revision_note.clone(),
                                    true,
                                ) {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        return Err(CodeUiApiError::conflict(
                                            "INTERACTION_NOT_ACTIVE",
                                            format!(
                                                "interaction '{interaction_id}' is already durably resolved with a different Modify note"
                                            ),
                                        )
                                        .into());
                                    }
                                    Err(verification_error) => {
                                        return Err(self
                                            .fence_consumed_phase1_response(
                                                runtime,
                                                session,
                                                &runtime_turn_id,
                                                &verification_error,
                                            )
                                            .await);
                                    }
                                }
                            }
                        // The response reply was lost after the worker had
                        // already committed its terminal resolution. Resume
                        // the idempotent closeout/handoff instead of rolling
                        // back the replacement generation.
                    }
                    (Some(requested), Some(durable)) => {
                        let conflict = anyhow!(
                            "interaction '{interaction_id}' is already durably resolved as '{durable}', not requested '{requested}'"
                        );
                        return Err(CodeUiApiError::conflict(
                            "INTERACTION_NOT_ACTIVE",
                            conflict.to_string(),
                        )
                        .into());
                    }
                    _ => {
                        // UnknownInteraction is not proof of non-consumption:
                        // a concurrent response clears the live interaction
                        // before its terminal+resolution fsync completes.
                        // Preserve every seed/provisional marker and fence
                        // rather than rolling back authority that may already
                        // be accepted.
                        let ambiguity = anyhow!(
                            "AgentRuntime response delivery failed without a durable terminal resolution; session requires reconciliation: {error}"
                        );
                        return Err(self
                            .fence_consumed_phase1_response(
                                runtime,
                                session,
                                &runtime_turn_id,
                                &ambiguity,
                            )
                            .await);
                    }
                }
            }
        }
        // The worker's combined terminal+resolution fsync is the first point
        // at which Cancel/Modify may change the browser projection or durable
        // revision sidecar. A failed combined append therefore leaves the
        // original pending gate intact for restart reconciliation.
        if intent_review_decision
            == Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Revise)
            && let Err(error) = super::headless::enter_web_intent_revision_mode(
                session,
                self.persistence.as_ref(),
                &self.pending_intent_revision,
                interaction_id,
                &runtime_turn_id,
                intent_revision_note.clone(),
            )
            .await
        {
            let error = anyhow!(error);
            return Err(self
                .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                .await);
        }

        if let Some(confirmed) = confirmed_intent.as_ref() {
            let phase1_turn_id = confirmed.phase1_turn_id_override.as_ref().ok_or_else(|| {
                anyhow!("Intent Confirm is missing its durable Phase 1 attempt id")
            })?;
            // Install the pre-admission owner while the response transition is
            // still serialized. If the HTTP future is aborted after enqueue,
            // Cancel and shutdown can still mark this exact attempt before the
            // coordinator observes it; Start reuses these shared markers.
            self.active_turn_mutations
                .lock()
                .await
                .entry(phase1_turn_id.clone())
                .or_insert_with(|| Arc::new(AtomicBool::new(false)));
            self.phase1_attempt_states
                .lock()
                .await
                .entry(phase1_turn_id.clone())
                .or_insert_with(|| Arc::new(AtomicU8::new(PHASE1_ATTEMPT_PLANNING)));
        }

        let handoff_result: anyhow::Result<()> = async {
            if let Some(confirmed_intent) = confirmed_intent {
                let (admitted_tx, admitted_rx) = tokio::sync::oneshot::channel();
                self.send_phase1_command(HeadlessPhase1Command::Start {
                    confirmed: confirmed_intent,
                    admitted: Some(admitted_tx),
                    start: None,
                })
                .await?;
                admitted_rx
                    .await
                    .map_err(|_| anyhow!("Phase 1 coordinator stopped before Phase 1 admission"))?
                    .map_err(|error| anyhow!(error))?;
            }
            if let Some(prepared) = prepared_network {
                let (reply, parked) = tokio::sync::oneshot::channel();
                self.send_phase1_command(HeadlessPhase1Command::ParkNetwork { prepared, reply })
                    .await?;
                parked
                    .await
                    .map_err(|_| anyhow!("Phase 1 coordinator dropped network gate park"))?
                    .map_err(|error| anyhow!(error))?;
            }
            if let Some(prepared) = prepared_plan_back {
                let (reply, parked) = tokio::sync::oneshot::channel();
                self.send_phase1_command(HeadlessPhase1Command::ParkPlanBack { prepared, reply })
                    .await?;
                parked
                    .await
                    .map_err(|_| anyhow!("Phase 1 coordinator dropped Plan Back park"))?
                    .map_err(|error| anyhow!(error))?;
            }
            if network_allow {
                let (reply, started) = tokio::sync::oneshot::channel();
                self.send_phase1_command(HeadlessPhase1Command::StartPlanExecution {
                    network_interaction_id: interaction_id.to_string(),
                    reply,
                })
                .await?;
                started
                    .await
                    .map_err(|_| {
                        anyhow!("Phase 1 coordinator dropped confirmed plan-execution start")
                    })?
                    .map_err(|error| anyhow!(error))?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = handoff_result {
            return Err(self
                .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                .await);
        }
        if is_intent_review {
            session.resolve_interaction(interaction_id).await;
            if intent_review_decision
                != Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Confirm)
            {
                session.set_status(CodeUiSessionStatus::Idle).await;
            }
            if let Some(persistence) = self.persistence.as_ref()
                && let Err(error) = persistence.persist_snapshot(session.snapshot().await).await
            {
                let error = anyhow!(error);
                return Err(self
                    .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                    .await);
            }
            if intent_review_decision
                != Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Confirm)
                && let Some(persistence) = self.persistence.as_ref()
            {
                crate::internal::ai::runtime::phase1::clear_phase1_start_seed(
                    &persistence.goal_event_store(),
                )?;
            }
            self.pending_intent_reviews
                .lock()
                .await
                .remove(&runtime_turn_id);
            self.active_turn_mutations
                .lock()
                .await
                .remove(&runtime_turn_id);
            release_web_turn(&self.in_flight, &runtime_turn_id).await;
        }
        let terminal_plan_decision = matches!(
            plan_decision,
            Some(
                crate::internal::ai::runtime::phase1::PlanReviewDecision::Revise
                    | crate::internal::ai::runtime::phase1::PlanReviewDecision::Cancel
            )
        );
        let terminal_network_deny = is_network_policy
            && selected_option.as_deref().is_some_and(|id| {
                crate::internal::ai::runtime::phase1::NetworkPolicyDecision::from_wire_id(id)
                    == Some(crate::internal::ai::runtime::phase1::NetworkPolicyDecision::Deny)
            });
        if terminal_plan_decision || terminal_network_deny {
            let admission = self.clone();
            let runtime = runtime.clone();
            let session = session.clone();
            let interaction_id = interaction_id.to_string();
            let runtime_turn_id_for_task = runtime_turn_id.clone();
            let remove_context = terminal_network_deny
                || plan_decision
                    == Some(crate::internal::ai::runtime::phase1::PlanReviewDecision::Cancel);
            let finalizer = tokio::spawn(async move {
                session.resolve_interaction(&interaction_id).await;
                session.set_status(CodeUiSessionStatus::Idle).await;
                if let Some(persistence) = admission.persistence.as_ref()
                    && let Err(error) = persistence.persist_snapshot(session.snapshot().await).await
                {
                    let error = anyhow!(error);
                    return Err(admission
                        .fence_consumed_phase1_response(
                            &runtime,
                            &session,
                            &runtime_turn_id_for_task,
                            &error,
                        )
                        .await);
                }
                release_web_turn(&admission.in_flight, &runtime_turn_id_for_task).await;
                admission
                    .active_turn_mutations
                    .lock()
                    .await
                    .remove(&runtime_turn_id_for_task);
                admission
                    .phase1_attempt_states
                    .lock()
                    .await
                    .remove(&runtime_turn_id_for_task);
                if remove_context
                    && let (Some(persistence), Some(context_id)) = (
                        admission.persistence.as_ref(),
                        terminal_phase1_context_id.as_ref(),
                    )
                    && let Err(error) =
                        crate::internal::ai::runtime::phase1::clear_phase1_review_context(
                            &persistence.goal_event_store(),
                            context_id,
                        )
                {
                    tracing::warn!(
                        context_id,
                        %error,
                        "failed to garbage collect terminal Phase 1 context; sidecar retained for later recovery cleanup"
                    );
                }
                Ok(())
            });
            return finalizer
                .await
                .map_err(|error| anyhow!("Phase 1 terminal gate finalizer stopped: {error}"))?;
        }
        Ok(())
    }

    async fn resolve_phase1_retry_before_cancel_ack(
        &self,
        phase1_turn_id: &str,
        seed: &crate::internal::ai::runtime::phase1::Phase1StartSeed,
        wait_for_admission: bool,
    ) -> anyhow::Result<()> {
        let persistence = self
            .persistence
            .as_ref()
            .ok_or_else(|| anyhow!("Phase 1 cancellation requires durable session persistence"))?;
        let store = persistence.goal_event_store();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let replay = store.load_code_workflow_replay_committed()?;
            if !crate::internal::ai::runtime::phase1::phase1_source_resolution_matches_seed(
                replay.events.iter().map(|event| &event.event),
                seed,
            ) {
                return Err(anyhow!(
                    "Phase 1 cancellation cannot verify the start seed's durable source resolution"
                ));
            }
            match crate::internal::ai::runtime::phase1::phase1_retry_intent_review_state(
                replay.events.iter().map(|event| &event.event),
                phase1_turn_id,
            )? {
                crate::internal::ai::runtime::phase1::Phase1RetryIntentReviewState::NoIntent
                    if !wait_for_admission =>
                {
                    return Ok(());
                }
                crate::internal::ai::runtime::phase1::Phase1RetryIntentReviewState::NoIntent
                | crate::internal::ai::runtime::phase1::Phase1RetryIntentReviewState::PendingTerminal =>
                {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(anyhow!(
                            "Timed out waiting for Phase 1 command '{phase1_turn_id}' to publish its durable terminal before acknowledging cancellation"
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                crate::internal::ai::runtime::phase1::Phase1RetryIntentReviewState::TerminalWithoutRetry => {
                    return Ok(());
                }
                crate::internal::ai::runtime::phase1::Phase1RetryIntentReviewState::Open(
                    review,
                ) => {
                    crate::internal::ai::runtime::phase1::validate_phase1_retry_intent_review_for_seed(
                        &review,
                        phase1_turn_id,
                        seed,
                    )?;
                    store.append_code_workflow_durable(
                        crate::internal::ai::session::CodeWorkflowEventKind::InteractionResolved {
                            interaction_id: review.interaction_id,
                            resolution: "cancel".to_string(),
                            command: None,
                            prior_interaction_resolutions: Vec::new(),
                            intent_revision_consumption: None,
                        },
                    )?;
                    return Ok(());
                }
                crate::internal::ai::runtime::phase1::Phase1RetryIntentReviewState::Resolved {
                    review,
                    resolution,
                } => {
                    crate::internal::ai::runtime::phase1::validate_phase1_retry_intent_review_for_seed(
                        &review,
                        phase1_turn_id,
                        seed,
                    )?;
                    if crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(
                        &resolution,
                    ) == Some(crate::internal::ai::runtime::phase0::IntentReviewDecision::Cancel)
                    {
                        return Ok(());
                    }
                    return Err(CodeUiApiError::conflict(
                        "INTERACTION_NOT_ACTIVE",
                        format!(
                            "Phase 1 retry interaction '{}' is already resolved as '{}', not cancel",
                            review.interaction_id, resolution
                        ),
                    )
                    .into());
                }
            }
        }
    }

    pub(crate) async fn cancel_turn(
        &self,
        runtime: &AgentRuntimeHandle,
        session: &Arc<CodeUiSession>,
        interaction_id_lookup: impl FnOnce(
            &crate::internal::ai::runtime::InteractionState,
        ) -> Option<&str>,
    ) -> anyhow::Result<()> {
        let _transition = self.interaction_transition.lock().await;
        self.ensure_not_shutting_down()?;
        let runtime_turn_id = {
            let slot = self.in_flight.lock().await;
            slot.as_ref().map(|turn| turn.runtime_turn_id.clone())
        };
        let Some(runtime_turn_id) = runtime_turn_id else {
            // W5-02: cancel with no turn in flight is a state conflict, not
            // a silent success — the state matrix pins 409 SESSION_BUSY
            // (the wire contract the TUI bridge used to serve).
            return Err(super::code_ui::CodeUiApiError::conflict(
                "SESSION_BUSY",
                "No turn is currently running; there is nothing to cancel",
            )
            .into());
        };
        let registered_runtime_interaction_id = runtime
            .snapshot(self.runtime_session_id.clone())
            .await
            .ok()
            .and_then(|snapshot| {
                interaction_id_lookup(&snapshot.interaction).map(ToOwned::to_owned)
            });
        let web_snapshot = session.snapshot().await;
        let pending_intent_review_ids = web_snapshot
            .interactions
            .iter()
            .filter(|interaction| {
                interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
            .map(|interaction| interaction.id.clone())
            .collect::<Vec<_>>();
        let phase1_start_seed = if let Some(persistence) = self.persistence.as_ref() {
            crate::internal::ai::runtime::phase1::load_phase1_start_seed(
                &persistence.goal_event_store(),
            )?
        } else {
            None
        };
        let phase1_seed_turn_id = phase1_start_seed
            .as_ref()
            .map(crate::internal::ai::runtime::phase1::phase1_turn_id_from_seed)
            .transpose()?;
        let (phase1_attempt_turn_id, phase1_attempt_state) = {
            let attempts = self.phase1_attempt_states.lock().await;
            if let Some(state) = attempts.get(&runtime_turn_id) {
                (Some(runtime_turn_id.clone()), Some(state.clone()))
            } else if let Some(turn_id) = phase1_seed_turn_id.as_ref()
                && let Some(state) = attempts.get(turn_id)
            {
                (Some(turn_id.clone()), Some(state.clone()))
            } else {
                (None, None)
            }
        };
        // A durable Intent gate in the Web projection remains authoritative
        // even if the runtime snapshot momentarily reports another interaction
        // while the executor is crossing the registration boundary. Never
        // downgrade that gate to ordinary turn cancellation: doing so can ACK
        // without the combined terminal + resolution fsync.
        let pending_intent_review_auth = if !pending_intent_review_ids.is_empty() {
            let Some(persistence) = self.persistence.as_ref() else {
                return Err(anyhow!(
                    "Unable to verify the pending IntentSpec review before cancellation because durable session persistence is unavailable"
                ));
            };
            let replay = persistence
                .goal_event_store()
                .load_code_workflow_replay_committed()?;
            let open = crate::internal::ai::runtime::phase0::open_intent_review_from_workflow(
                replay.events.iter().map(|event| &event.event),
            );
            match open {
                Some((interaction_id, _, stored_turn_id, phase0_turn_id))
                    if pending_intent_review_ids.len() == 1
                        && pending_intent_review_ids[0] == interaction_id
                        && (phase0_turn_id == runtime_turn_id
                            || stored_turn_id == runtime_turn_id) =>
                {
                    (Some(interaction_id), false)
                }
                None if pending_intent_review_ids.len() == 1 => {
                    let pending_interaction_id = &pending_intent_review_ids[0];
                    let exact_consumed_confirm_source = if let (
                        Some(seed),
                        Some(seed_turn_id),
                        Some(attempt_turn_id),
                        Some(attempt_state),
                    ) = (
                        phase1_start_seed.as_ref(),
                        phase1_seed_turn_id.as_ref(),
                        phase1_attempt_turn_id.as_ref(),
                        phase1_attempt_state.as_ref(),
                    ) {
                        let same_source =
                            seed.source_interaction_id.as_str() == pending_interaction_id.as_str();
                        let same_attempt = seed_turn_id == attempt_turn_id;
                        let source_is_confirm =
                            crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(
                                &seed.source_resolution,
                            ) == Some(
                                crate::internal::ai::runtime::phase0::IntentReviewDecision::Confirm,
                            );
                        let source_resolution_matches =
                            crate::internal::ai::runtime::phase1::phase1_source_resolution_matches_seed(
                                replay.events.iter().map(|event| &event.event),
                                seed,
                            );
                        let attempt_is_cancellable = matches!(
                            attempt_state.load(Ordering::Acquire),
                            PHASE1_ATTEMPT_PLANNING | PHASE1_ATTEMPT_ADMITTING
                        );
                        same_source
                            && same_attempt
                            && source_is_confirm
                            && source_resolution_matches
                            && attempt_is_cancellable
                    } else {
                        false
                    };
                    if !exact_consumed_confirm_source {
                        return Err(anyhow!(
                            "Unable to authenticate the pending IntentSpec review against its durable Phase 0 command before cancellation"
                        ));
                    }
                    // Confirm has already atomically closed the source gate, but
                    // its browser close-out can be aborted after the derived
                    // Phase 1 Start is enqueued. Cancel the authenticated Phase 1
                    // owner below; responding Cancel to the closed source would
                    // conflict with its durable Confirm first-writer.
                    (None, true)
                }
                _ => {
                    return Err(anyhow!(
                        "Unable to authenticate the pending IntentSpec review against its durable Phase 0 command before cancellation"
                    ));
                }
            }
        } else {
            (None, false)
        };
        let (authenticated_pending_intent_review_id, stale_consumed_intent_projection) =
            pending_intent_review_auth;
        // Intent control Cancel always uses the same response first-writer as
        // an ordinary browser Cancel. This is safe both before and after
        // runtime registration and avoids a snapshot-dependent split between
        // response terminalization and generic cancellation.
        let intent_review_cancel_via_response = authenticated_pending_intent_review_id.is_some();
        let runtime_interaction_id = if stale_consumed_intent_projection {
            None
        } else {
            authenticated_pending_intent_review_id
                .clone()
                .or(registered_runtime_interaction_id)
        };
        let durable_review_cancel = if let Some(interaction_id) = runtime_interaction_id.as_ref() {
            web_snapshot.interactions.iter().any(|interaction| {
                &interaction.id == interaction_id
                    && matches!(
                        interaction.kind,
                        super::code_ui::CodeUiInteractionKind::IntentReviewChoice
                            | super::code_ui::CodeUiInteractionKind::PostPlanChoice
                    )
            })
        } else {
            false
        };
        let cancelled_phase1_context_id = if durable_review_cancel {
            if let (Some(persistence), Some(interaction_id)) =
                (self.persistence.as_ref(), runtime_interaction_id.as_ref())
            {
                let store = persistence.goal_event_store();
                let replay = store.load_code_workflow_replay()?;
                crate::internal::ai::runtime::phase1::phase1_context_id_for_gate_interaction(
                    replay.events.iter().map(|event| &event.event),
                    interaction_id,
                )
            } else {
                None
            }
        } else {
            None
        };

        let phase1_cancelled_from = phase1_attempt_state.as_ref().and_then(|state| {
            loop {
                let current = state.load(Ordering::Acquire);
                if !matches!(current, PHASE1_ATTEMPT_PLANNING | PHASE1_ATTEMPT_ADMITTING) {
                    break None;
                }
                match state.compare_exchange(
                    current,
                    PHASE1_ATTEMPT_CANCELLED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break Some(current),
                    Err(_) => continue,
                }
            }
        });
        let phase1_cancel_won = phase1_cancelled_from.is_some();
        let mutation_in_progress = if let Some(state) = phase1_attempt_state.as_ref() {
            // A Phase 1 tool call may set the generic tool-loop mutation flag
            // while it is only constructing a draft. The explicit attempt
            // state is the sole authority for whether the formal plan-write
            // boundary was crossed.
            state.load(Ordering::Acquire) == PHASE1_ATTEMPT_MUTATING
        } else {
            self.active_turn_mutations
                .lock()
                .await
                .get(
                    phase1_attempt_turn_id
                        .as_deref()
                        .unwrap_or(runtime_turn_id.as_str()),
                )
                .is_some_and(|marker| marker.load(Ordering::Acquire))
        };
        let cancel_runtime_turn_id = phase1_attempt_turn_id
            .as_ref()
            .unwrap_or(&runtime_turn_id)
            .clone();
        let cancel_result = if intent_review_cancel_via_response {
            let interaction_id = runtime_interaction_id.as_ref().ok_or_else(|| {
                anyhow!("pre-registration IntentSpec cancellation lost its interaction id")
            })?;
            let response = serde_json::to_string(&CodeUiInteractionResponse {
                selected_option: Some("cancel".to_string()),
                ..Default::default()
            })
            .map_err(|error| {
                anyhow!("Unable to encode the pre-registration IntentSpec cancellation: {error}")
            })?;
            // The UI projection can become visible before the executor reports
            // AwaitingInteraction. Runtime's response first-writer covers both
            // that early window and an already-registered Intent gate, holding
            // this acknowledgement until the combined terminal+resolution
            // fsync has completed.
            runtime
                .respond(
                    self.runtime_session_id.clone(),
                    runtime_turn_id.clone(),
                    crate::internal::ai::runtime::InteractionResponse::new(
                        interaction_id,
                        response,
                    ),
                )
                .await
        } else if durable_review_cancel {
            let interaction_id = runtime_interaction_id.as_ref().ok_or_else(|| {
                anyhow!("durable review cancellation lost its interaction identifier")
            })?;
            runtime
                .cancel_interaction(
                    self.runtime_session_id.clone(),
                    runtime_turn_id.clone(),
                    interaction_id.clone(),
                    "cancel",
                )
                .await
        } else {
            runtime
                .cancel(
                    self.runtime_session_id.clone(),
                    cancel_runtime_turn_id.clone(),
                )
                .await
        };
        match cancel_result {
            Ok(()) => {}
            Err(RuntimeWorkerError::UnknownTurn { .. }) if durable_review_cancel => {
                let exact_resolution = if let (Some(persistence), Some(interaction_id)) =
                    (self.persistence.as_ref(), runtime_interaction_id.as_ref())
                {
                    match persistence
                        .goal_event_store()
                        .load_code_workflow_replay_committed()
                    {
                        Ok(replay) => replay.events.iter().any(|event| {
                                matches!(
                                    &event.event,
                                    crate::internal::ai::session::CodeWorkflowEventKind::InteractionResolved {
                                        interaction_id: candidate,
                                        resolution,
                                        intent_revision_consumption: None,
                                        ..
                                    }
                                    | crate::internal::ai::session::CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                                        interaction_id: candidate,
                                        resolution,
                                        ..
                                    } if candidate == interaction_id && resolution == "cancel"
                                )
                            }),
                        Err(replay_error) => {
                            let error = anyhow!(
                                "Unable to verify the durable review-gate cancellation after live ownership was lost: {replay_error}"
                            );
                            return Err(self
                                .fence_consumed_phase1_response(
                                    runtime,
                                    session,
                                    &runtime_turn_id,
                                    &error,
                                )
                                .await);
                        }
                    }
                } else {
                    false
                };
                if !exact_resolution {
                    let error = anyhow!(
                        "review-gate cancellation lost live ownership without a matching durable cancellation resolution"
                    );
                    return Err(self
                        .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                        .await);
                }
            }
            Err(RuntimeWorkerError::UnknownTurn { .. }) => {}
            Err(error @ RuntimeWorkerError::ReconciliationRequired { .. })
                if durable_review_cancel =>
            {
                let error = anyhow!(
                    "Unable to durably cancel the review gate; session requires reconciliation: {}",
                    runtime_worker_adapter_message(error)
                );
                return Err(self
                    .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                    .await);
            }
            Err(error @ RuntimeWorkerError::ReconciliationRequired { .. }) => {
                return Err(error.into());
            }
            Err(error @ RuntimeWorkerError::DurabilityFailure(_)) if durable_review_cancel => {
                let error = anyhow!(
                    "Unable to durably cancel the review gate; session requires reconciliation: {}",
                    runtime_worker_adapter_message(error)
                );
                return Err(self
                    .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                    .await);
            }
            Err(error) => {
                return Err(anyhow!(
                    "Unable to request cancellation from the AgentRuntime: {}",
                    runtime_worker_adapter_message(error)
                ));
            }
        }
        if intent_review_cancel_via_response {
            let interaction_id =
                authenticated_pending_intent_review_id
                    .as_ref()
                    .ok_or_else(|| {
                        anyhow!(
                            "authenticated IntentSpec cancellation lost its interaction identifier"
                        )
                    })?;
            let persistence = self.persistence.as_ref().ok_or_else(|| {
                anyhow!(
                    "Unable to verify the durable IntentSpec cancellation because session persistence is unavailable"
                )
            })?;
            let exact_combined_terminals = match persistence
                .goal_event_store()
                .load_code_workflow_replay_committed()
            {
                Ok(replay) => replay
                    .events
                    .iter()
                    .filter(|event| {
                        matches!(
                            &event.event,
                            crate::internal::ai::session::CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                                command,
                                interaction_id: candidate,
                                resolution,
                                ..
                            } if command.command_id == runtime_turn_id
                                && candidate == interaction_id
                                && resolution == "cancel"
                        )
                    })
                    .count(),
                Err(replay_error) => {
                    let error = anyhow!(
                        "Unable to verify the durable IntentSpec cancellation after the runtime acknowledged it: {replay_error}"
                    );
                    return Err(self
                        .fence_consumed_phase1_response(
                            runtime,
                            session,
                            &runtime_turn_id,
                            &error,
                        )
                        .await);
                }
            };
            if exact_combined_terminals != 1 {
                let error = anyhow!(
                    "AgentRuntime acknowledged the IntentSpec cancellation without exactly one matching durable combined terminal"
                );
                return Err(self
                    .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                    .await);
            }
        }
        let phase1_cancelled_before_mutation = phase1_cancel_won
            && phase1_seed_turn_id.as_deref() == Some(cancel_runtime_turn_id.as_str());
        let mut phase1_terminal_idle = false;
        if phase1_cancelled_before_mutation && let Some(persistence) = self.persistence.as_ref() {
            let store = persistence.goal_event_store();
            let seed = phase1_start_seed.as_ref().ok_or_else(|| {
                anyhow!(
                    "Unable to verify the durable Phase 1 start seed before acknowledging cancellation"
                )
            })?;
            self.resolve_phase1_retry_before_cancel_ack(
                &cancel_runtime_turn_id,
                seed,
                phase1_cancelled_from == Some(PHASE1_ATTEMPT_ADMITTING),
            )
            .await?;
            let worker_snapshot = match runtime.snapshot(self.runtime_session_id.clone()).await {
                Ok(snapshot) => snapshot,
                Err(runtime_error) => {
                    let error = anyhow!(
                        "Unable to verify that the cancelled Phase 1 attempt reached a terminal runtime state: {}",
                        runtime_worker_adapter_message(runtime_error)
                    );
                    return Err(self
                        .fence_consumed_phase1_response(
                            runtime,
                            session,
                            &cancel_runtime_turn_id,
                            &error,
                        )
                        .await);
                }
            };
            let worker_idle = worker_snapshot.active_turn_id.is_none();
            if !worker_idle {
                let error = anyhow!(
                    "Cancelled Phase 1 attempt '{}' remained active after its durable terminal was acknowledged",
                    cancel_runtime_turn_id
                );
                return Err(self
                    .fence_consumed_phase1_response(
                        runtime,
                        session,
                        &cancel_runtime_turn_id,
                        &error,
                    )
                    .await);
            }
            session.clear_interaction(&seed.source_interaction_id).await;
            if let Some(interaction_id) = runtime_interaction_id.as_ref() {
                session.clear_interaction(interaction_id).await;
            }
            super::headless::finalize_terminal_phase1_projection(
                session,
                "Phase 1 planning cancelled before any formal write",
                "cancelled",
            )
            .await;
            if let Err(error) = persistence.persist_snapshot(session.snapshot().await).await {
                let error = anyhow!(
                    "Unable to durably finalize the cancelled Phase 1 projection before consuming its start seed: {error}"
                );
                return Err(self
                    .fence_consumed_phase1_response(
                        runtime,
                        session,
                        &cancel_runtime_turn_id,
                        &error,
                    )
                    .await);
            }
            if let Err(error) =
                crate::internal::ai::runtime::phase1::clear_phase1_start_seed(&store)
            {
                let error =
                    anyhow!("Unable to durably consume the cancelled Phase 1 start seed: {error}");
                return Err(self
                    .fence_consumed_phase1_response(runtime, session, &runtime_turn_id, &error)
                    .await);
            }
            if let Some(state) = phase1_attempt_state.as_ref() {
                // The transition guard keeps the detached settlement blocked
                // until this store. The FIFO cleanup command below owns final
                // tombstone removal; settlement must not repeat the fallible
                // snapshot/seed close-out after Cancel earns its 2xx.
                state.store(PHASE1_ATTEMPT_SETTLED, Ordering::Release);
            }
            if let Err(error) = self
                .send_phase1_command(HeadlessPhase1Command::CleanupAttempt {
                    phase1_turn_id: cancel_runtime_turn_id.clone(),
                })
                .await
            {
                // Retaining the terminal tombstone is fail-closed: no later
                // same-generation Start may recreate Planning. The entry is
                // bounded to this process and disappears on shutdown.
                tracing::warn!(
                    phase1_turn_id = %cancel_runtime_turn_id,
                    %error,
                    "failed to enqueue Phase 1 attempt cleanup barrier; retaining terminal tombstone"
                );
            }
            phase1_terminal_idle = true;
        }
        if let Some(interaction_id) = runtime_interaction_id {
            session.clear_interaction(&interaction_id).await;
        } else if phase1_cancelled_before_mutation && let Some(seed) = phase1_start_seed.as_ref() {
            session.clear_interaction(&seed.source_interaction_id).await;
        }
        if mutation_in_progress && !intent_review_cancel_via_response {
            // W5-02 / ADR-CODE-05 (W1-04): the cooperative cancellation was
            // already requested from the runtime above; a mutating tool is
            // never hard-aborted, the turn settles once the tool reports a
            // determinate result. Refusing HERE misreported an accepted
            // cooperative cancel as an HTTP error and broke the pinned
            // `state_cancel_while_executing_tool_settles_running_tool_call`
            // matrix case — acceptance is the contract.
            return Ok(());
        }
        // Parked IntentSpec review (and other WaitingActive gates) finish
        // synchronously in the worker with no executor path left to call
        // `release_web_turn`. Clear the admission slot so the next submit is
        // not blocked by a ghost in-flight turn.
        // A successful Intent-review response ACK is issued only after the
        // worker has terminalized the exact command and cleared its active
        // owner. The mutation flag captured before that await can still be the
        // sticky Phase 0 drafting marker, so it must not strand the Web slot.
        let worker_idle = phase1_terminal_idle
            || intent_review_cancel_via_response
            || runtime
                .snapshot(self.runtime_session_id.clone())
                .await
                .ok()
                .is_some_and(|snapshot| snapshot.active_turn_id.is_none());
        if worker_idle {
            release_web_turn(&self.in_flight, &runtime_turn_id).await;
            if !phase1_terminal_idle {
                self.active_turn_mutations
                    .lock()
                    .await
                    .remove(&runtime_turn_id);
                self.phase1_attempt_states
                    .lock()
                    .await
                    .remove(&runtime_turn_id);
            }
            // Cancel never reaches settle_plan_phase0_intent_review; drop the
            // parked IntentSpec review entry here so draft→cancel loops cannot
            // retain unbounded UUID keys for the process lifetime.
            self.pending_intent_reviews
                .lock()
                .await
                .remove(&runtime_turn_id);
            if !phase1_terminal_idle {
                if !matches!(
                    session.snapshot().await.status,
                    CodeUiSessionStatus::IndeterminateSideEffect
                ) {
                    session.set_status(CodeUiSessionStatus::Idle).await;
                }
                if let Some(persistence) = self.persistence.as_ref()
                    && let Err(error) = persistence.persist_snapshot(session.snapshot().await).await
                {
                    session
                        .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                        .await;
                    let _ = persistence.persist_snapshot(session.snapshot().await).await;
                    return Err(anyhow!(
                        "Unable to persist session after cancelling a parked IntentSpec review; session requires reconciliation: {error}"
                    ));
                }
            }
            if let (Some(persistence), Some(context_id)) = (
                self.persistence.as_ref(),
                cancelled_phase1_context_id.as_ref(),
            ) && let Err(error) =
                crate::internal::ai::runtime::phase1::clear_phase1_review_context(
                    &persistence.goal_event_store(),
                    context_id,
                )
            {
                tracing::warn!(
                    context_id,
                    %error,
                    "failed to garbage collect globally cancelled Phase 1 context; sidecar retained for later recovery cleanup"
                );
            }
        }
        Ok(())
    }

    /// W4-13: drop persisted tool-approval / user-input prompts after a
    /// controller lease takeover. Intent/Plan/network-policy gates stay.
    pub(crate) async fn clear_pending_tool_interactions(
        &self,
        session: &Arc<CodeUiSession>,
    ) -> anyhow::Result<()> {
        let cleared = session.clear_pending_tool_interactions().await;
        if cleared == 0 {
            return Ok(());
        }
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };
        persistence
            .persist_snapshot(session.snapshot().await)
            .await
            .map_err(|error| {
                anyhow!(
                    "failed to persist Code UI snapshot after lease-takeover interaction drop: {error}"
                )
            })?;
        Ok(())
    }

    async fn cancel_gated_runtime_turn(
        &self,
        runtime: &AgentRuntimeHandle,
        runtime_turn_id: &str,
        completion: Arc<tokio::sync::Notify>,
    ) -> anyhow::Result<()> {
        // Register before sending cancellation: Notify does not retain a
        // broadcast for a future, unregistered waiter. Bound the wait as a
        // second line of defence so a damaged worker cannot leave admission
        // wedged after a failed durable preflight.
        let cancellation_finished = completion.notified();
        tokio::pin!(cancellation_finished);
        cancellation_finished.as_mut().enable();
        match runtime
            .cancel(self.runtime_session_id.clone(), runtime_turn_id.to_string())
            .await
        {
            Ok(()) => {
                const GATED_CANCEL_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
                if tokio::time::timeout(GATED_CANCEL_SETTLE_TIMEOUT, cancellation_finished.as_mut())
                    .await
                    .is_err()
                {
                    release_web_turn(&self.in_flight, runtime_turn_id).await;
                    return Err(anyhow!(
                        "The cancelled AgentRuntime turn did not settle within {} seconds; its browser admission was released and the runtime should be inspected before retrying",
                        GATED_CANCEL_SETTLE_TIMEOUT.as_secs()
                    ));
                }
            }
            Err(RuntimeWorkerError::UnknownTurn { .. }) => {
                release_web_turn(&self.in_flight, runtime_turn_id).await;
            }
            Err(error @ RuntimeWorkerError::ReconciliationRequired { .. }) => {
                return Err(error.into());
            }
            Err(error) => {
                return Err(anyhow!(
                    "The gated AgentRuntime turn could not be cancelled; no tool gate was opened. Verify runtime/session storage before retrying: {}",
                    runtime_worker_adapter_message(error)
                ));
            }
        }
        Ok(())
    }

    async fn rearm_cancelled_revision_if_present(
        &self,
        session: &Arc<CodeUiSession>,
        claiming_revision: Option<&(PendingIntentRevision, IntentRevisionConsumptionClaim)>,
    ) -> anyhow::Result<()> {
        let Some((pending, claim)) = claiming_revision else {
            return Ok(());
        };
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };
        if let Err(error) = rearm_cancelled_intent_revision_consumer(persistence, pending, claim) {
            session
                .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                .await;
            return Err(anyhow!(
                "The pre-start command was cancelled, but its IntentSpec revision consumer could not be safely rearmed; restart to reconcile before retrying: {error}"
            ));
        }
        Ok(())
    }
}

pub(crate) async fn release_web_turn(
    in_flight: &Mutex<Option<InFlightTurn>>,
    runtime_turn_id: &str,
) {
    let completion = {
        let mut slot = in_flight.lock().await;
        if slot
            .as_ref()
            .is_some_and(|turn| turn.runtime_turn_id == runtime_turn_id)
        {
            slot.take().map(|turn| turn.completion)
        } else {
            None
        }
    };
    if let Some(completion) = completion {
        completion.notify_waiters();
    }
}

pub(crate) async fn wait_for_web_turn_start(
    start_gate: &tokio::sync::Notify,
    start_open: &AtomicBool,
    cancellation: tokio_util::sync::CancellationToken,
) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        if start_open.load(Ordering::Acquire) {
            return true;
        }
        let notified = start_gate.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if start_open.load(Ordering::Acquire) {
            return true;
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return false,
            _ = &mut notified => {}
        }
    }
}

fn intent_review_resolution_label(response: &CodeUiInteractionResponse) -> Option<&'static str> {
    response
        .selected_option
        .as_deref()
        .and_then(crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id)
        .map(|decision| decision.wire_id())
}

fn workflow_resolution_label(response: &CodeUiInteractionResponse) -> Option<&'static str> {
    let selected = response.selected_option.as_deref()?;
    crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(selected)
        .map(|decision| decision.wire_id())
        .or_else(|| {
            crate::internal::ai::runtime::phase1::NetworkPolicyDecision::from_wire_id(selected)
                .map(|decision| decision.wire_id())
        })
        .or_else(|| {
            crate::internal::ai::runtime::phase1::PlanReviewDecision::from_wire_id(selected)
                .map(|decision| decision.wire_id())
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkflowGateKind {
    Intent,
    Plan,
    Network,
}

fn workflow_gate_kind_for_interaction<'a>(
    events: impl DoubleEndedIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
    interaction_id: &str,
) -> Option<WorkflowGateKind> {
    events.rev().find_map(|event| match event {
        crate::internal::ai::session::CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: candidate,
            ..
        } if candidate == interaction_id => Some(WorkflowGateKind::Intent),
        crate::internal::ai::session::CodeWorkflowEventKind::CommandTerminalFailure {
            retry_intent_review: Some(retry),
            ..
        } if retry.interaction_id == interaction_id => Some(WorkflowGateKind::Intent),
        crate::internal::ai::session::CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: candidate,
            ..
        } if candidate == interaction_id => Some(WorkflowGateKind::Plan),
        crate::internal::ai::session::CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: candidate,
            ..
        } if candidate == interaction_id => Some(WorkflowGateKind::Network),
        _ => None,
    })
}

fn workflow_resolutions_match(kind: WorkflowGateKind, requested: &str, durable: &str) -> bool {
    match kind {
        WorkflowGateKind::Intent =>
            crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(requested)
                .is_some_and(|decision| {
                    Some(decision)
                        == crate::internal::ai::runtime::phase0::IntentReviewDecision::from_wire_id(
                            durable,
                        )
                }),
        WorkflowGateKind::Plan =>
            crate::internal::ai::runtime::phase1::PlanReviewDecision::from_wire_id(requested)
                .is_some_and(|decision| {
                    Some(decision)
                        == crate::internal::ai::runtime::phase1::PlanReviewDecision::from_wire_id(
                            durable,
                        )
                }),
        WorkflowGateKind::Network =>
            crate::internal::ai::runtime::phase1::NetworkPolicyDecision::from_wire_id(requested)
                .is_some_and(|decision| {
                    Some(decision)
                        == crate::internal::ai::runtime::phase1::NetworkPolicyDecision::from_wire_id(
                            durable,
                        )
                }),
    }
}
