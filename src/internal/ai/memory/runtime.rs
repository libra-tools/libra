//! Production activation seam for repository Memory.
//!
//! Callers provide the active completion model and a provider-aware context
//! budget once. This module hides observer repair, bounded job consumption,
//! deterministic recall query construction, receipt persistence, and prompt
//! delivery behind two operations: [`MemoryRuntime::maintain`] and
//! [`MemoryRuntime::prepare_context`].

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use chrono::Utc;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    compiler::{
        EpisodeCompileConfig, EpisodeCompiler, EpisodeCompilerSet,
        intent::{
            INTENT_ITERATION_PROMPT_VERSION, INTENT_ITERATION_RULES_VERSION,
            IntentIterationCompiler,
        },
        task::{TASK_EPISODE_PROMPT_VERSION, TASK_EPISODE_RULES_VERSION, TaskEpisodeCompiler},
    },
    domain::{ActorKind, ActorRefV1},
    job::repair_observers_with_digest,
    limits::EpisodeSourceLimits,
    policy::{AuthenticatedMemoryContext, REPO_EPISODE_PRODUCER},
    query::EpisodeQueryV1,
    runner::{EpisodeGenerationRunner, GenerationRunOutcome},
    writer::MemoryWriter,
};
use crate::{
    internal::ai::{
        completion::CompletionModel,
        context_budget::{
            AuditedMemoryContextBundleV1, ContextBudget,
            memory::{MemoryContextAssembler, MemoryContextAssemblerErrorKind},
        },
        history::HistoryManager,
        keyed_digest::RepositoryKeyedDigest,
    },
    utils::util::DATABASE,
};

const MEMORY_SCOPE_KEY: &str = "repo";
const RUNTIME_PRINCIPAL_ID: &str = "libra-memory-runtime";
const MAX_GENERATIONS_PER_WAKE: usize = 4;
const MAX_RUNTIME_QUERY_BYTES: usize = 4 * 1024;
const MAX_RUNTIME_QUERY_TERM_BYTES: usize = 256;
const MAX_RUNTIME_QUERY_TERMS: usize = 32;
const MAINTENANCE_RUNNING: u8 = 1;
const MAINTENANCE_DIRTY: u8 = 1 << 1;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MemoryMaintenanceReport {
    pub(crate) attempted: usize,
    pub(crate) committed: usize,
    pub(crate) retry_scheduled: usize,
    pub(crate) stable_failures: usize,
    pub(crate) fenced_out: usize,
    pub(crate) generations_pending: usize,
}

/// Session-local owner for automatic Memory generation and recall.
///
/// The worker is deliberately bounded and has no polling loop of its own.
/// Agent runtimes wake it at stable lifecycle points; durable jobs and leases
/// preserve progress across crashes and process restarts.
pub(crate) struct MemoryRuntime {
    history: Arc<HistoryManager>,
    digest: Arc<RepositoryKeyedDigest>,
    writer: Arc<MemoryWriter>,
    task_compiler: Box<dyn EpisodeCompiler>,
    intent_compiler: Box<dyn EpisodeCompiler>,
    task_config: EpisodeCompileConfig,
    intent_config: EpisodeCompileConfig,
    context_budget: ContextBudget,
    owner: String,
    maintenance_gate: Mutex<()>,
    maintenance_schedule: AtomicU8,
    #[cfg(test)]
    maintenance_worker_spawns: AtomicUsize,
}

impl MemoryRuntime {
    pub(crate) async fn open<M>(
        history: Arc<HistoryManager>,
        model: M,
        model_id: impl Into<String>,
        context_budget: ContextBudget,
    ) -> Result<Self, MemoryRuntimeError>
    where
        M: CompletionModel + Clone + Send + Sync + 'static,
    {
        let model_id = model_id.into();
        let storage_path = history.repository_path().to_path_buf();
        let database_path = storage_path.join(DATABASE);
        let digest = RepositoryKeyedDigest::load_or_initialize(&database_path)
            .await
            .map_err(|_| MemoryRuntimeError::storage())?;
        let writer = Arc::new(
            MemoryWriter::open(storage_path)
                .await
                .map_err(|_| MemoryRuntimeError::storage())?,
        );
        Self::from_dependencies(history, digest, writer, model, model_id, context_budget)
    }

    fn from_dependencies<M>(
        history: Arc<HistoryManager>,
        digest: Arc<RepositoryKeyedDigest>,
        writer: Arc<MemoryWriter>,
        model: M,
        model_id: String,
        context_budget: ContextBudget,
    ) -> Result<Self, MemoryRuntimeError>
    where
        M: CompletionModel + Clone + Send + Sync + 'static,
    {
        let task_compiler = TaskEpisodeCompiler::with_model_id(model.clone(), model_id.clone())
            .map_err(|_| MemoryRuntimeError::configuration())?;
        let intent_compiler = IntentIterationCompiler::with_model_id(model, model_id.clone())
            .map_err(|_| MemoryRuntimeError::configuration())?;
        let task_config = EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            TASK_EPISODE_RULES_VERSION,
            TASK_EPISODE_PROMPT_VERSION,
            model_id.clone(),
        )
        .map_err(|_| MemoryRuntimeError::configuration())?;
        let intent_config = EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            INTENT_ITERATION_RULES_VERSION,
            INTENT_ITERATION_PROMPT_VERSION,
            model_id,
        )
        .map_err(|_| MemoryRuntimeError::configuration())?;
        Ok(Self {
            history,
            digest,
            writer,
            task_compiler: Box::new(task_compiler),
            intent_compiler: Box::new(intent_compiler),
            task_config,
            intent_config,
            context_budget,
            owner: format!("libra-memory-{}", Uuid::new_v4()),
            maintenance_gate: Mutex::new(()),
            maintenance_schedule: AtomicU8::new(0),
            #[cfg(test)]
            maintenance_worker_spawns: AtomicUsize::new(0),
        })
    }

    #[cfg(test)]
    fn for_tests<M>(
        history: Arc<HistoryManager>,
        digest: Arc<RepositoryKeyedDigest>,
        writer: Arc<MemoryWriter>,
        model: M,
        model_id: impl Into<String>,
        context_budget: ContextBudget,
    ) -> Result<Self, MemoryRuntimeError>
    where
        M: CompletionModel + Clone + Send + Sync + 'static,
    {
        Self::from_dependencies(
            history,
            digest,
            writer,
            model,
            model_id.into(),
            context_budget,
        )
    }

    /// Reconcile terminal events and consume at most four durable generations.
    pub(crate) async fn maintain(&self) -> Result<MemoryMaintenanceReport, MemoryRuntimeError> {
        let _maintenance_guard = self.maintenance_gate.lock().await;
        repair_observers_with_digest(self.history.as_ref(), self.digest.as_ref())
            .await
            .map_err(|_| MemoryRuntimeError::observer_repair())?;
        let database = self.history.database_connection();
        let runner = EpisodeGenerationRunner::new(
            self.history.as_ref(),
            &database,
            self.digest.as_ref(),
            self.writer.as_ref(),
            MEMORY_SCOPE_KEY,
            EpisodeSourceLimits::repo_v1(),
        )
        .map_err(|_| MemoryRuntimeError::configuration())?;
        let compilers = EpisodeCompilerSet::new(
            self.task_compiler.as_ref(),
            &self.task_config,
            self.intent_compiler.as_ref(),
            &self.intent_config,
        );
        let mut report = MemoryMaintenanceReport::default();
        for _ in 0..MAX_GENERATIONS_PER_WAKE {
            let now_ms = Utc::now().timestamp_millis();
            let outcome = runner
                .run_one(&compilers, &self.owner, now_ms)
                .await
                .map_err(|_| MemoryRuntimeError::generation())?;
            if outcome == GenerationRunOutcome::NoWork {
                break;
            }
            report.attempted = report.attempted.saturating_add(1);
            match outcome {
                GenerationRunOutcome::NoWork => {}
                GenerationRunOutcome::Committed {
                    new_generation_pending,
                    ..
                } => {
                    report.committed = report.committed.saturating_add(1);
                    if new_generation_pending {
                        report.generations_pending = report.generations_pending.saturating_add(1);
                    }
                }
                GenerationRunOutcome::RetryScheduled => {
                    report.retry_scheduled = report.retry_scheduled.saturating_add(1);
                }
                GenerationRunOutcome::StableFailure => {
                    report.stable_failures = report.stable_failures.saturating_add(1);
                }
                GenerationRunOutcome::FencedOut => {
                    report.fenced_out = report.fenced_out.saturating_add(1);
                }
                GenerationRunOutcome::NewGenerationPending => {
                    report.generations_pending = report.generations_pending.saturating_add(1);
                }
            }
        }
        Ok(report)
    }

    /// Wake one coalescing worker without extending the Agent request's
    /// critical path. Repeated wakes set a dirty bit rather than creating
    /// tasks that wait on the maintenance mutex. A wake arriving during a run
    /// therefore requests at most one additional bounded pass.
    pub(crate) fn schedule_maintenance(self: &Arc<Self>) {
        let previous = self
            .maintenance_schedule
            .fetch_or(MAINTENANCE_DIRTY, Ordering::AcqRel);
        if previous & MAINTENANCE_RUNNING != 0 {
            return;
        }
        if self
            .maintenance_schedule
            .compare_exchange(
                MAINTENANCE_DIRTY,
                MAINTENANCE_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }

        let runtime = Arc::clone(self);
        #[cfg(test)]
        runtime
            .maintenance_worker_spawns
            .fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            loop {
                match runtime.maintain().await {
                    Ok(report) if report.attempted > 0 => {
                        tracing::debug!(
                            attempted = report.attempted,
                            committed = report.committed,
                            retry_scheduled = report.retry_scheduled,
                            stable_failures = report.stable_failures,
                            fenced_out = report.fenced_out,
                            generations_pending = report.generations_pending,
                            "completed bounded Memory maintenance wake"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            kind = ?error.kind(),
                            "Memory maintenance wake failed; durable work remains available for retry"
                        );
                    }
                }

                let previous = runtime
                    .maintenance_schedule
                    .fetch_and(!MAINTENANCE_DIRTY, Ordering::AcqRel);
                if previous & MAINTENANCE_DIRTY != 0 {
                    continue;
                }
                match runtime.maintenance_schedule.compare_exchange(
                    MAINTENANCE_RUNNING,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(current) if current & MAINTENANCE_DIRTY != 0 => continue,
                    Err(_) => break,
                }
            }
        });
    }

    /// Build and durably receipt the Memory section for one Agent request.
    /// Empty or punctuation-only prompts produce no Memory query.
    pub(crate) async fn prepare_context(
        &self,
        prompt: &str,
    ) -> Result<Option<String>, MemoryRuntimeError> {
        let Some(text) = bounded_recall_text_v1(prompt) else {
            return Ok(None);
        };
        let context = AuthenticatedMemoryContext::new(
            self.digest.repository_id(),
            ActorRefV1 {
                kind: ActorKind::System,
                principal_id: RUNTIME_PRINCIPAL_ID.to_string(),
            },
        )
        .map_err(|_| MemoryRuntimeError::configuration())?;
        let query = EpisodeQueryV1 {
            text: Some(text),
            ..EpisodeQueryV1::default()
        };
        let bundle = MemoryContextAssembler::new(self.history.as_ref(), Arc::clone(&self.digest))
            .assemble(&context, &query, &self.context_budget, Utc::now())
            .await
            .map_err(|error| match error.kind() {
                MemoryContextAssemblerErrorKind::Receipt
                | MemoryContextAssemblerErrorKind::ReceiptStore => {
                    MemoryRuntimeError::receipt_persistence()
                }
                _ => MemoryRuntimeError::recall(),
            })?;
        prepared_context(bundle)
    }
}

fn prepared_context(
    bundle: AuditedMemoryContextBundleV1,
) -> Result<Option<String>, MemoryRuntimeError> {
    let selected_count = bundle.receipt().selected().len();
    if selected_count == 0 {
        return Ok(None);
    }
    if bundle.prompt_section().trim().is_empty() {
        return Err(MemoryRuntimeError::recall());
    }
    Ok(Some(bundle.prompt_section().to_string()))
}

/// Reduce an arbitrary Agent request to the bounded plain-text query accepted
/// by the FTS5 reader. Input order is retained; duplicate terms use ASCII
/// case-folding only, matching the reader's normalization contract.
fn bounded_recall_text_v1(input: &str) -> Option<String> {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut bytes = 0usize;
    for term in input.split(|character: char| !character.is_alphanumeric()) {
        if term.is_empty() || term.len() > MAX_RUNTIME_QUERY_TERM_BYTES {
            continue;
        }
        let key = term.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let separator = usize::from(!selected.is_empty());
        if selected.len() == MAX_RUNTIME_QUERY_TERMS
            || bytes.saturating_add(separator).saturating_add(term.len()) > MAX_RUNTIME_QUERY_BYTES
        {
            break;
        }
        bytes = bytes.saturating_add(separator).saturating_add(term.len());
        selected.push(term);
    }
    (!selected.is_empty()).then(|| selected.join(" "))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryRuntimeErrorKind {
    InvalidConfiguration,
    StorageUnavailable,
    ObserverRepairFailed,
    GenerationFailed,
    RecallFailed,
    ReceiptPersistenceFailed,
}

#[derive(Debug, Error)]
#[error("Memory runtime failed ({kind:?})")]
pub(crate) struct MemoryRuntimeError {
    kind: MemoryRuntimeErrorKind,
}

impl MemoryRuntimeError {
    const fn new(kind: MemoryRuntimeErrorKind) -> Self {
        Self { kind }
    }

    const fn configuration() -> Self {
        Self::new(MemoryRuntimeErrorKind::InvalidConfiguration)
    }

    const fn storage() -> Self {
        Self::new(MemoryRuntimeErrorKind::StorageUnavailable)
    }

    const fn observer_repair() -> Self {
        Self::new(MemoryRuntimeErrorKind::ObserverRepairFailed)
    }

    const fn generation() -> Self {
        Self::new(MemoryRuntimeErrorKind::GenerationFailed)
    }

    const fn recall() -> Self {
        Self::new(MemoryRuntimeErrorKind::RecallFailed)
    }

    const fn receipt_persistence() -> Self {
        Self::new(MemoryRuntimeErrorKind::ReceiptPersistenceFailed)
    }

    pub(crate) const fn kind(&self) -> MemoryRuntimeErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use git_internal::internal::object::{
        ObjectTrait,
        task::Task,
        task_event::{TaskEvent, TaskEventKind},
        types::ActorRef,
    };
    use sea_orm::{ConnectionTrait, Statement};
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        internal::ai::{
            completion::{
                AssistantContent, CompletionError, CompletionRequest, CompletionResponse, Message,
                OneOrMany, Text, UserContent,
            },
            memory::writer::tests::fixture,
        },
        utils::{object::write_git_object, storage::local::LocalStorage},
    };

    #[derive(Clone)]
    struct TaskPromptModel;

    impl CompletionModel for TaskPromptModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let Message::User { content } = request
                .chat_history
                .last()
                .expect("Memory compiler emits one user message")
            else {
                panic!("Memory compiler must emit a user message");
            };
            let OneOrMany::One(UserContent::Text(input)) = content else {
                panic!("Memory compiler must emit one text input");
            };
            let input: serde_json::Value =
                serde_json::from_str(&input.text).expect("parse Memory compiler input");
            let fragment_id = input["fragments"][0]["fragment_id"]
                .as_str()
                .expect("resolved source has a fragment ID");
            let reply = serde_json::json!({
                "summary": {
                    "epistemic_status": "inference",
                    "claim": "the terminal evidence is ready for reuse",
                    "confidence": "medium",
                    "evidence_fragment_ids": [fragment_id]
                },
                "observations": [{
                    "epistemic_status": "observation",
                    "claim": "the task reached a terminal state",
                    "confidence": null,
                    "evidence_fragment_ids": [fragment_id]
                }],
                "inferences": [],
                "decisions": [],
                "failed_attempts": [],
                "unresolved": []
            })
            .to_string();
            Ok(CompletionResponse {
                content: vec![AssistantContent::Text(Text { text: reply })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct BlockingTaskPromptModel {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        calls: Arc<AtomicUsize>,
    }

    impl CompletionModel for BlockingTaskPromptModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.entered.notify_one();
            self.release.notified().await;
            TaskPromptModel.completion(request).await
        }
    }

    async fn append_object<T: ObjectTrait>(
        history: &HistoryManager,
        object_type: &str,
        object_id: &str,
        object: &T,
    ) {
        let bytes = object.to_data().expect("serialize AI history object");
        let oid = write_git_object(history.repository_path(), "blob", &bytes)
            .expect("write AI history blob");
        history
            .append(object_type, object_id, oid)
            .await
            .expect("append AI history object");
    }

    #[test]
    fn runtime_recall_query_is_bounded_deduplicated_and_deterministic() {
        let input = (0..48)
            .map(|index| format!("Term{index}"))
            .chain(["TERM1".to_string(), "memory".to_string()])
            .collect::<Vec<_>>()
            .join(" / ");
        let query = bounded_recall_text_v1(&input).expect("query has searchable terms");
        let terms = query.split_whitespace().collect::<Vec<_>>();
        assert_eq!(terms.len(), MAX_RUNTIME_QUERY_TERMS);
        assert_eq!(terms[0], "Term0");
        assert_eq!(terms[31], "Term31");
        assert_eq!(
            bounded_recall_text_v1(&input).as_deref(),
            Some(query.as_str())
        );
        assert_eq!(bounded_recall_text_v1("---\n\t"), None);
    }

    #[tokio::test]
    async fn runtime_repairs_observers_and_consumes_terminal_task_once() {
        let fixture = fixture().await;
        let history = Arc::new(HistoryManager::new(
            Arc::new(LocalStorage::new(fixture._temp.path().join("objects"))),
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        ));
        let actor = ActorRef::agent("runtime-test-agent").expect("test actor");
        let task =
            Task::new(actor.clone(), "compile a runtime episode", None).expect("construct Task");
        let task_id = task.header().object_id();
        append_object(history.as_ref(), "task", &task_id.to_string(), &task).await;
        let done = TaskEvent::new(actor, task_id, TaskEventKind::Done)
            .expect("construct terminal Task event");
        append_object(
            history.as_ref(),
            "task_event",
            &done.header().object_id().to_string(),
            &done,
        )
        .await;

        let runtime = MemoryRuntime::for_tests(
            Arc::clone(&history),
            Arc::clone(&fixture.digest),
            Arc::clone(&fixture.writer),
            TaskPromptModel,
            "runtime-fake",
            ContextBudget::default(),
        )
        .expect("construct test Memory runtime");
        let first = runtime.maintain().await.expect("run Memory maintenance");
        assert_eq!(first.attempted, 1);
        assert_eq!(first.committed, 1);
        assert_eq!(first.retry_scheduled, 0);
        assert_eq!(first.stable_failures, 0);

        let second = runtime.maintain().await.expect("repeat Memory maintenance");
        assert_eq!(second, MemoryMaintenanceReport::default());

        let row = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT COUNT(*) AS note_count FROM memory_head WHERE scope_key = 'repo'",
                [],
            ))
            .await
            .expect("query generated Memory head")
            .expect("count row exists");
        assert_eq!(row.try_get::<i64>("", "note_count").unwrap(), 1);
    }

    #[tokio::test]
    async fn scheduled_maintenance_coalesces_wakes_into_one_worker() {
        let fixture = fixture().await;
        let history = Arc::new(HistoryManager::new(
            Arc::new(LocalStorage::new(fixture._temp.path().join("objects"))),
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        ));
        let actor = ActorRef::agent("runtime-coalescing-agent").expect("test actor");
        let task =
            Task::new(actor.clone(), "coalesce maintenance wakes", None).expect("construct Task");
        let task_id = task.header().object_id();
        append_object(history.as_ref(), "task", &task_id.to_string(), &task).await;
        let done = TaskEvent::new(actor, task_id, TaskEventKind::Done)
            .expect("construct terminal Task event");
        append_object(
            history.as_ref(),
            "task_event",
            &done.header().object_id().to_string(),
            &done,
        )
        .await;

        let model = BlockingTaskPromptModel {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let entered = model.entered.notified();
        let runtime = Arc::new(
            MemoryRuntime::for_tests(
                Arc::clone(&history),
                Arc::clone(&fixture.digest),
                Arc::clone(&fixture.writer),
                model.clone(),
                "runtime-blocking-fake",
                ContextBudget::default(),
            )
            .expect("construct test Memory runtime"),
        );
        runtime.schedule_maintenance();
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .expect("maintenance reached the compiler");

        for _ in 0..128 {
            runtime.schedule_maintenance();
        }
        assert_eq!(
            runtime.maintenance_worker_spawns.load(Ordering::Relaxed),
            1,
            "repeated wakes must not enqueue mutex-waiting Tokio tasks"
        );
        assert_eq!(model.calls.load(Ordering::Relaxed), 1);
        model.release.notify_one();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let row = fixture
                    .database
                    .query_one_raw(Statement::from_string(
                        fixture.database.get_database_backend(),
                        "SELECT COUNT(*) AS note_count FROM memory_head WHERE scope_key = 'repo'"
                            .to_string(),
                    ))
                    .await
                    .expect("query generated Memory head")
                    .expect("count row exists");
                if row.try_get::<i64>("", "note_count").unwrap() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("coalesced worker completed");
    }

    #[tokio::test]
    async fn runtime_classifies_receipt_write_failure_as_fail_loud() {
        use crate::internal::ai::memory::{
            domain::MemorySensitivity,
            reader::tests::{commit_injectable_episode, history, seed_code_head},
        };

        let fixture = fixture().await;
        let code_commit = seed_code_head(&fixture).await;
        commit_injectable_episode(
            &fixture,
            code_commit,
            "task-runtime-receipt-failure",
            1,
            MemorySensitivity::Internal,
            "runtimereceiptfailuretoken",
            "This candidate must not cross a failed runtime receipt gate.",
        )
        .await;
        let history = Arc::new(history(&fixture));
        let runtime = MemoryRuntime::for_tests(
            history,
            Arc::clone(&fixture.digest),
            Arc::clone(&fixture.writer),
            TaskPromptModel,
            "runtime-fake",
            ContextBudget::default(),
        )
        .expect("construct test Memory runtime");
        fixture
            .database
            .execute_unprepared("DROP TABLE context_selection_receipt")
            .await
            .expect("remove fixture receipt ledger");

        let error = runtime
            .prepare_context("runtimereceiptfailuretoken")
            .await
            .expect_err("receipt failure must abort runtime context preparation");
        assert_eq!(
            error.kind(),
            MemoryRuntimeErrorKind::ReceiptPersistenceFailed
        );
    }
}
