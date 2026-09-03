use git_internal::internal::object::{
    ObjectTrait,
    intent_event::{IntentEvent, IntentEventKind},
    task_event::{TaskEvent, TaskEventKind},
    types::ActorKind as HistoryActorKind,
};
use sea_orm::DatabaseConnection;
use thiserror::Error;

use super::{
    admission::{EpisodeAdmission, EpisodeAdmissionErrorKind},
    compiler::{EpisodeCompiler, EpisodeCompilerSet},
    domain::{ActorKind, ActorRefV1, EpisodeRootKind},
    error::MemoryWriterErrorKind,
    job_sql::{claim_next_job, complete_job, record_job_failure, release_job_dirty},
    job_state::{
        CompileFailureClass, CompileJobCompletionOutcome, CompileJobMutationOutcome,
        StableJobFailure,
    },
    limits::EpisodeSourceLimits,
    observer::{MemoryDependencyObserver, canonical_intent_input},
    policy::{AuthenticatedMemoryContext, TrustedMemoryTarget},
    source::{EpisodeSourceErrorKind, EpisodeSourceResolver},
    writer::MemoryWriter,
};
use crate::internal::ai::{history::HistoryManager, keyed_digest::RepositoryKeyedDigest};

const TERMINAL_APPEND_ANCESTRY_LIMIT: usize = 2_048;
const TERMINAL_APPEND_TREE_BYTES: u64 = 4 * 1024 * 1024;
const TERMINAL_APPEND_BLOB_BYTES: u64 = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GenerationRunOutcome {
    NoWork,
    Committed {
        appended: bool,
        new_generation_pending: bool,
    },
    RetryScheduled,
    StableFailure,
    FencedOut,
    NewGenerationPending,
}

/// Drives exactly one claimed generation through the complete Memory write
/// path. Looping, concurrency, and runtime deadlines stay outside this deep
/// module so they cannot create a second authority or bypass lease fencing.
pub(crate) struct EpisodeGenerationRunner<'a> {
    history: &'a HistoryManager,
    database: &'a DatabaseConnection,
    digest: &'a RepositoryKeyedDigest,
    writer: &'a MemoryWriter,
    scope_key: &'a str,
    source_limits: EpisodeSourceLimits,
}

impl<'a> EpisodeGenerationRunner<'a> {
    pub(crate) fn new(
        history: &'a HistoryManager,
        database: &'a DatabaseConnection,
        digest: &'a RepositoryKeyedDigest,
        writer: &'a MemoryWriter,
        scope_key: &'a str,
        source_limits: EpisodeSourceLimits,
    ) -> Result<Self, EpisodeGenerationRunnerError> {
        source_limits
            .validate()
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
        if scope_key != "repo" {
            return Err(EpisodeGenerationRunnerError::configuration());
        }
        Ok(Self {
            history,
            database,
            digest,
            writer,
            scope_key,
            source_limits,
        })
    }

    pub(crate) async fn run_one<T: EpisodeCompiler + ?Sized, I: EpisodeCompiler + ?Sized>(
        &self,
        compilers: &EpisodeCompilerSet<'_, T, I>,
        owner: &str,
        now_ms: i64,
    ) -> Result<GenerationRunOutcome, EpisodeGenerationRunnerError> {
        let Some(lease) = claim_next_job(self.database, self.scope_key, owner, now_ms)
            .await
            .map_err(|_| EpisodeGenerationRunnerError::job())?
        else {
            return Ok(GenerationRunOutcome::NoWork);
        };

        let actor = match self.terminal_actor(&lease).await {
            Ok(actor) => actor,
            Err(failure) => return self.record_failure(&lease, failure, now_ms).await,
        };
        let context = AuthenticatedMemoryContext::new(self.digest.repository_id(), actor)
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
        let target = TrustedMemoryTarget::episode(lease.key().root().clone());
        let resolver = EpisodeSourceResolver::new(self.history, self.digest, self.source_limits)
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
        let source = match resolver
            .resolve(&context, &target, lease.terminal_source_oid())
            .await
        {
            Ok(source) => source,
            Err(error) => {
                let failure = source_failure(error.kind())?;
                return self.record_failure(&lease, failure, now_ms).await;
            }
        };
        if lease.key().root().kind() == EpisodeRootKind::Intent
            && !self.intent_lease_matches(&lease, &source)?
        {
            let observer = MemoryDependencyObserver::new(
                self.history,
                self.database,
                self.digest,
                self.scope_key,
            )
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
            if observer.observe_task_revisions().await.is_err() {
                return self
                    .record_failure(
                        &lease,
                        failure(
                            CompileFailureClass::Transient,
                            "LBR-MEMORY-201",
                            "Intent dependencies could not be refreshed",
                        )?,
                        now_ms,
                    )
                    .await;
            }
            return match release_job_dirty(self.database, &lease, now_ms)
                .await
                .map_err(|_| EpisodeGenerationRunnerError::job())?
            {
                CompileJobMutationOutcome::Applied => {
                    Ok(GenerationRunOutcome::NewGenerationPending)
                }
                CompileJobMutationOutcome::FencedOut => Ok(GenerationRunOutcome::FencedOut),
            };
        }
        let admission = EpisodeAdmission::new(self.digest);
        let admitted_result = match lease.key().root().kind() {
            EpisodeRootKind::Task => {
                let (compiler, config) = compilers.task();
                admission
                    .compile(compiler, config, &context, &target, source)
                    .await
            }
            EpisodeRootKind::Intent => {
                let (compiler, config) = compilers.intent();
                admission
                    .compile(compiler, config, &context, &target, source)
                    .await
            }
        };
        let admitted = match admitted_result {
            Ok(admitted) => admitted,
            Err(error) => {
                let failure = admission_failure(error.kind())?;
                return self.record_failure(&lease, failure, now_ms).await;
            }
        };
        let committed = match self
            .writer
            .commit_admitted(&resolver, &context, &target, &admitted, None, Some(&lease))
            .await
        {
            Ok(committed) => committed,
            Err(error) => {
                let failure = writer_failure(error.kind())?;
                return self.record_failure(&lease, failure, now_ms).await;
            }
        };
        if let Ok(observer) =
            MemoryDependencyObserver::new(self.history, self.database, self.digest, self.scope_key)
            && observer.observe_task_revisions().await.is_err()
        {
            tracing::warn!(
                "Memory dependency observer failed after a committed generation; repair will retry"
            );
        }
        match complete_job(self.database, &lease, now_ms)
            .await
            .map_err(|_| EpisodeGenerationRunnerError::job())?
        {
            CompileJobCompletionOutcome::Clean => Ok(GenerationRunOutcome::Committed {
                appended: committed.appended(),
                new_generation_pending: false,
            }),
            CompileJobCompletionOutcome::NewGenerationPending => {
                Ok(GenerationRunOutcome::Committed {
                    appended: committed.appended(),
                    new_generation_pending: true,
                })
            }
            CompileJobCompletionOutcome::FencedOut => Ok(GenerationRunOutcome::FencedOut),
        }
    }

    fn intent_lease_matches(
        &self,
        lease: &super::job_state::CompileJobLease,
        source: &super::source::RedactedEpisodeSource,
    ) -> Result<bool, EpisodeGenerationRunnerError> {
        let revisions = source
            .pinned_task_episodes()
            .iter()
            .map(|task| {
                (
                    task.task_id().to_string(),
                    Some(task.revision_oid().to_string()),
                )
            })
            .collect::<Vec<_>>();
        let fingerprint = self
            .digest
            .source_input_fingerprint(&canonical_intent_input(
                lease.key().root(),
                lease.terminal_source_oid(),
                &revisions,
            ))
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
        Ok(&fingerprint == lease.input_fingerprint())
    }

    async fn terminal_actor(
        &self,
        lease: &super::job_state::CompileJobLease,
    ) -> Result<ActorRefV1, StableJobFailure> {
        let append = self
            .history
            .read_append_at(
                lease.terminal_source_oid(),
                TERMINAL_APPEND_ANCESTRY_LIMIT,
                TERMINAL_APPEND_TREE_BYTES,
                TERMINAL_APPEND_BLOB_BYTES,
            )
            .await
            .map_err(|_| stable_failure("LBR-MEMORY-202", "terminal source is unavailable"))?;
        let history_actor = match lease.key().root().kind() {
            EpisodeRootKind::Task if append.object_type() == "task_event" => {
                let event =
                    TaskEvent::from_bytes(append.bytes(), append.object_oid()).map_err(|_| {
                        stable_failure("LBR-MEMORY-202", "terminal Task event is invalid")
                    })?;
                if event.task_id().to_string() != lease.key().root().id()
                    || !matches!(
                        event.kind(),
                        TaskEventKind::Done | TaskEventKind::Failed | TaskEventKind::Cancelled
                    )
                {
                    return Err(stable_failure(
                        "LBR-MEMORY-202",
                        "terminal Task source does not match the claimed root",
                    ));
                }
                event.header().created_by().clone()
            }
            EpisodeRootKind::Intent if append.object_type() == "intent_event" => {
                let event =
                    IntentEvent::from_bytes(append.bytes(), append.object_oid()).map_err(|_| {
                        stable_failure("LBR-MEMORY-202", "terminal Intent event is invalid")
                    })?;
                if event.intent_id().to_string() != lease.key().root().id()
                    || !matches!(
                        event.kind(),
                        IntentEventKind::Completed | IntentEventKind::Cancelled
                    )
                {
                    return Err(stable_failure(
                        "LBR-MEMORY-202",
                        "terminal Intent source does not match the claimed root",
                    ));
                }
                event.header().created_by().clone()
            }
            _ => {
                return Err(stable_failure(
                    "LBR-MEMORY-202",
                    "terminal source type does not match the claimed root",
                ));
            }
        };
        let kind = match history_actor.kind() {
            HistoryActorKind::Human => ActorKind::Human,
            HistoryActorKind::Agent | HistoryActorKind::McpClient => ActorKind::Agent,
            HistoryActorKind::System | HistoryActorKind::Other(_) => ActorKind::System,
        };
        Ok(ActorRefV1 {
            kind,
            principal_id: history_actor.id().to_string(),
        })
    }

    async fn record_failure(
        &self,
        lease: &super::job_state::CompileJobLease,
        failure: StableJobFailure,
        now_ms: i64,
    ) -> Result<GenerationRunOutcome, EpisodeGenerationRunnerError> {
        let class = failure.class();
        match record_job_failure(self.database, lease, &failure, now_ms)
            .await
            .map_err(|_| EpisodeGenerationRunnerError::job())?
        {
            CompileJobMutationOutcome::FencedOut => Ok(GenerationRunOutcome::FencedOut),
            CompileJobMutationOutcome::Applied => Ok(match class {
                CompileFailureClass::Transient => GenerationRunOutcome::RetryScheduled,
                CompileFailureClass::Stable => GenerationRunOutcome::StableFailure,
            }),
        }
    }
}

fn source_failure(
    kind: EpisodeSourceErrorKind,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    let (class, code, summary) = match kind {
        EpisodeSourceErrorKind::SourceNotReachable => (
            CompileFailureClass::Transient,
            "LBR-MEMORY-201",
            "terminal source is not currently reachable",
        ),
        EpisodeSourceErrorKind::DependencyPending => (
            CompileFailureClass::Stable,
            "LBR-MEMORY-202",
            "required Task Episode revisions are not confirmed",
        ),
        EpisodeSourceErrorKind::Unauthorized
        | EpisodeSourceErrorKind::InvalidRequest
        | EpisodeSourceErrorKind::SourceCorrupt
        | EpisodeSourceErrorKind::LimitExceeded
        | EpisodeSourceErrorKind::RedactionFailed
        | EpisodeSourceErrorKind::DigestUnavailable => (
            CompileFailureClass::Stable,
            "LBR-MEMORY-202",
            "terminal source failed policy or validation",
        ),
    };
    failure(class, code, summary)
}

fn admission_failure(
    kind: EpisodeAdmissionErrorKind,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    let (class, code, summary) = match kind {
        EpisodeAdmissionErrorKind::CompilerTransient => (
            CompileFailureClass::Transient,
            "LBR-MEMORY-203",
            "Episode compiler provider failed",
        ),
        EpisodeAdmissionErrorKind::CompilerStable
        | EpisodeAdmissionErrorKind::InvalidProposal
        | EpisodeAdmissionErrorKind::SourceMismatch
        | EpisodeAdmissionErrorKind::DigestUnavailable => (
            CompileFailureClass::Stable,
            "LBR-MEMORY-204",
            "Episode compiler output failed deterministic admission",
        ),
    };
    failure(class, code, summary)
}

fn writer_failure(
    kind: MemoryWriterErrorKind,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    let transient = matches!(
        kind,
        MemoryWriterErrorKind::ProjectionStale
            | MemoryWriterErrorKind::StorageFailure
            | MemoryWriterErrorKind::ConflictExhausted
    );
    failure(
        if transient {
            CompileFailureClass::Transient
        } else {
            CompileFailureClass::Stable
        },
        if transient {
            "LBR-MEMORY-205"
        } else {
            "LBR-MEMORY-206"
        },
        if transient {
            "Memory writer encountered a transient local conflict"
        } else {
            "Memory writer rejected the admitted proposal"
        },
    )
}

fn stable_failure(code: &str, summary: &str) -> StableJobFailure {
    // INVARIANT: every call site uses a compile-time LBR-MEMORY-NNN code and
    // a short static diagnostic, both inside StableJobFailure's hard limits.
    StableJobFailure::new(CompileFailureClass::Stable, code, summary)
        .expect("runner stable failures use valid bounded diagnostics")
}

fn failure(
    class: CompileFailureClass,
    code: &str,
    summary: &str,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    StableJobFailure::new(class, code, summary)
        .map_err(|_| EpisodeGenerationRunnerError::configuration())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeGenerationRunnerErrorKind {
    InvalidConfiguration,
    JobState,
}

#[derive(Debug, Error)]
#[error("Memory generation runner failed ({kind:?})")]
pub(crate) struct EpisodeGenerationRunnerError {
    kind: EpisodeGenerationRunnerErrorKind,
}

impl EpisodeGenerationRunnerError {
    const fn configuration() -> Self {
        Self {
            kind: EpisodeGenerationRunnerErrorKind::InvalidConfiguration,
        }
    }

    const fn job() -> Self {
        Self {
            kind: EpisodeGenerationRunnerErrorKind::JobState,
        }
    }

    pub(crate) const fn kind(&self) -> EpisodeGenerationRunnerErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use git_internal::internal::object::{
        ObjectTrait,
        intent::Intent,
        intent_event::{IntentEvent, IntentEventKind},
        task::Task,
        task_event::{TaskEvent, TaskEventKind},
        types::ActorRef,
    };
    use sea_orm::{ConnectionTrait, Statement};

    use super::*;
    use crate::{
        internal::ai::{
            completion::{
                AssistantContent, CompletionError, CompletionModel, CompletionRequest,
                CompletionResponse, Message, OneOrMany, Text, UserContent,
            },
            memory::{
                compiler::{
                    EpisodeCompileConfig,
                    intent::{
                        INTENT_ITERATION_PROMPT_VERSION, INTENT_ITERATION_RULES_VERSION,
                        IntentIterationCompiler,
                    },
                    task::{
                        TASK_EPISODE_PROMPT_VERSION, TASK_EPISODE_RULES_VERSION,
                        TaskEpisodeCompiler,
                    },
                },
                domain::EpisodeRoot,
                observer::EpisodeObserver,
                policy::REPO_EPISODE_PRODUCER,
                store::read_memory_ref_head,
                tree::load_note_bytes,
                validation::parse_memory_note_v1,
                writer::tests::{fixture, proposal},
            },
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
                .expect("Task compiler emits one user message")
            else {
                panic!("Task compiler must emit a user message");
            };
            let OneOrMany::One(UserContent::Text(input)) = content else {
                panic!("Task compiler must emit one text input");
            };
            let input: serde_json::Value =
                serde_json::from_str(&input.text).expect("parse Task prompt input");
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
    struct IntentPromptModel;

    impl CompletionModel for IntentPromptModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let Message::User { content } = request
                .chat_history
                .last()
                .expect("Intent compiler emits one user message")
            else {
                panic!("Intent compiler must emit a user message");
            };
            let OneOrMany::One(UserContent::Text(input)) = content else {
                panic!("Intent compiler must emit one text input");
            };
            let input: serde_json::Value =
                serde_json::from_str(&input.text).expect("parse Intent prompt input");
            let task_fragments = input["task_episodes"]
                .as_array()
                .expect("Intent input has pinned Task Episodes")
                .iter()
                .map(|fragment| {
                    fragment["fragment_id"]
                        .as_str()
                        .expect("pinned Task summary has a fragment ID")
                })
                .collect::<Vec<_>>();
            let intent_fragment = input["intent_fragments"][0]["fragment_id"]
                .as_str()
                .expect("Intent input has a root fragment");
            let reply = serde_json::json!({
                "summary": {
                    "epistemic_status": "inference",
                    "claim": "the intent converged across all pinned task revisions",
                    "confidence": "medium",
                    "evidence_fragment_ids": task_fragments
                },
                "observations": [{
                    "epistemic_status": "observation",
                    "claim": "the parent intent reached a terminal state",
                    "confidence": null,
                    "evidence_fragment_ids": [intent_fragment]
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

    #[tokio::test]
    async fn generation_runner_resolves_compiles_and_commits_one_task() {
        let fixture = fixture().await;
        let storage = Arc::new(LocalStorage::new(fixture._temp.path().join("objects")));
        let history = HistoryManager::new(
            storage,
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        );
        let actor = ActorRef::agent("runner-test-agent").expect("test actor");
        let task =
            Task::new(actor.clone(), "compile a bounded episode", None).expect("construct task");
        let task_id = task.header().object_id();
        append_object(&history, "task", &task_id.to_string(), &task).await;
        let done = TaskEvent::new(actor, task_id, TaskEventKind::Done)
            .expect("construct terminal Task event");
        append_object(
            &history,
            "task_event",
            &done.header().object_id().to_string(),
            &done,
        )
        .await;

        EpisodeObserver::new(&history, fixture.database.as_ref(), &fixture.digest, "repo")
            .expect("construct terminal observer")
            .observe_terminal_events()
            .await
            .expect("observe terminal Task event");
        let config = EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            TASK_EPISODE_RULES_VERSION,
            TASK_EPISODE_PROMPT_VERSION,
            "deterministic-fake",
        )
        .expect("construct compile config");
        let compiler = TaskEpisodeCompiler::for_tests(TaskPromptModel, "deterministic-fake")
            .expect("construct Task Episode compiler");
        let compilers = EpisodeCompilerSet::new(&compiler, &config, &compiler, &config);
        let runner = EpisodeGenerationRunner::new(
            &history,
            fixture.database.as_ref(),
            &fixture.digest,
            &fixture.writer,
            "repo",
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct generation runner");
        assert_eq!(
            runner
                .run_one(&compilers, "runner-a", 2_000_000_000_000)
                .await
                .expect("run one generation"),
            GenerationRunOutcome::Committed {
                appended: true,
                new_generation_pending: false,
            }
        );

        let job = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT state, observed_generation, processed_generation, lease_owner
                 FROM memory_compile_job
                 WHERE scope_key = 'repo' AND root_kind = 'task' AND root_id = ?",
                [task_id.to_string().into()],
            ))
            .await
            .expect("query completed job")
            .expect("completed job exists");
        assert_eq!(job.try_get::<String>("", "state").unwrap(), "idle");
        assert_eq!(job.try_get::<i64>("", "observed_generation").unwrap(), 1);
        assert_eq!(job.try_get::<i64>("", "processed_generation").unwrap(), 1);
        assert_eq!(
            job.try_get::<Option<String>>("", "lease_owner").unwrap(),
            None
        );

        let memory = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT latest_review_state FROM memory_head
                 WHERE scope_key = 'repo' AND note_id = ?",
                [EpisodeRoot::task(task_id.to_string())
                    .unwrap()
                    .note_id()
                    .to_string()
                    .into()],
            ))
            .await
            .expect("query generated Memory head")
            .expect("generated Memory head exists");
        assert_eq!(
            memory.try_get::<String>("", "latest_review_state").unwrap(),
            "confirmed"
        );
        assert_eq!(
            runner
                .run_one(&compilers, "runner-a", 2_000_000_000_001)
                .await
                .expect("repeat completed generation"),
            GenerationRunOutcome::NoWork
        );

        let revised = TaskEvent::new(
            ActorRef::agent("runner-test-agent").expect("test actor"),
            task_id,
            TaskEventKind::Failed,
        )
        .expect("construct revised terminal Task event");
        append_object(
            &history,
            "task_event",
            &revised.header().object_id().to_string(),
            &revised,
        )
        .await;
        EpisodeObserver::new(&history, fixture.database.as_ref(), &fixture.digest, "repo")
            .expect("construct terminal observer")
            .observe_terminal_events()
            .await
            .expect("observe revised terminal Task event");
        assert_eq!(
            runner
                .run_one(&compilers, "runner-a", 2_000_000_000_002)
                .await
                .expect("run revised generation"),
            GenerationRunOutcome::Committed {
                appended: true,
                new_generation_pending: false,
            }
        );
        let revision_count = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT COUNT(*) AS revision_count FROM memory_revision_index
                 WHERE scope_key = 'repo' AND note_id = ?",
                [EpisodeRoot::task(task_id.to_string())
                    .unwrap()
                    .note_id()
                    .to_string()
                    .into()],
            ))
            .await
            .expect("query generated revisions")
            .expect("revision count row exists");
        assert_eq!(
            revision_count.try_get::<i64>("", "revision_count").unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn intent_generation_waits_for_pins_and_revises_after_task_change() {
        let fixture = fixture().await;
        let storage = Arc::new(LocalStorage::new(fixture._temp.path().join("objects")));
        let history = HistoryManager::new(
            storage,
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        );
        let actor = ActorRef::agent("intent-runner-agent").expect("test actor");
        let intent = Intent::new(actor.clone(), "deliver a version-bound memory iteration")
            .expect("construct Intent");
        let intent_id = intent.header().object_id();
        append_object(&history, "intent", &intent_id.to_string(), &intent).await;

        let mut task_ids = Vec::new();
        for title in ["implement source pins", "verify dependency wakeup"] {
            let mut task = Task::new(actor.clone(), title, None).expect("construct child Task");
            task.set_intent(Some(intent_id));
            let task_id = task.header().object_id();
            append_object(&history, "task", &task_id.to_string(), &task).await;
            let done = TaskEvent::new(actor.clone(), task_id, TaskEventKind::Done)
                .expect("construct terminal Task event");
            append_object(
                &history,
                "task_event",
                &done.header().object_id().to_string(),
                &done,
            )
            .await;
            task_ids.push(task_id);
        }
        let completed = IntentEvent::new(actor.clone(), intent_id, IntentEventKind::Completed)
            .expect("construct terminal Intent event");
        append_object(
            &history,
            "intent_event",
            &completed.header().object_id().to_string(),
            &completed,
        )
        .await;

        EpisodeObserver::new(&history, fixture.database.as_ref(), &fixture.digest, "repo")
            .expect("construct terminal observer")
            .observe_terminal_events()
            .await
            .expect("observe terminal Task and Intent events");
        let task_config = EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            TASK_EPISODE_RULES_VERSION,
            TASK_EPISODE_PROMPT_VERSION,
            "deterministic-task",
        )
        .expect("construct Task config");
        let intent_config = EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            INTENT_ITERATION_RULES_VERSION,
            INTENT_ITERATION_PROMPT_VERSION,
            "deterministic-intent",
        )
        .expect("construct Intent config");
        let task_compiler = TaskEpisodeCompiler::for_tests(TaskPromptModel, "deterministic-task")
            .expect("construct Task compiler");
        let intent_compiler =
            IntentIterationCompiler::for_tests(IntentPromptModel, "deterministic-intent")
                .expect("construct Intent compiler");
        let compilers = EpisodeCompilerSet::new(
            &task_compiler,
            &task_config,
            &intent_compiler,
            &intent_config,
        );
        let runner = EpisodeGenerationRunner::new(
            &history,
            fixture.database.as_ref(),
            &fixture.digest,
            &fixture.writer,
            "repo",
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct generation runner");

        assert_eq!(
            runner
                .run_one(&compilers, "intent-runner", 2_000_000_100_000)
                .await
                .expect("run missing-dependency Intent generation"),
            GenerationRunOutcome::StableFailure,
            "the lexically first Intent job must not publish before Task Episodes exist"
        );
        for offset in 1..=2 {
            assert_eq!(
                runner
                    .run_one(&compilers, "intent-runner", 2_000_000_100_000 + offset,)
                    .await
                    .expect("compile one child Task"),
                GenerationRunOutcome::Committed {
                    appended: true,
                    new_generation_pending: false,
                }
            );
        }
        let intent_root =
            EpisodeRoot::intent(intent_id.to_string()).expect("construct Intent root");
        let intent_target = TrustedMemoryTarget::episode(intent_root.clone());
        let resolver =
            EpisodeSourceResolver::new(&history, &fixture.digest, EpisodeSourceLimits::repo_v1())
                .expect("construct Intent source resolver");
        let terminal_source_oid = history
            .resolve_history_head()
            .await
            .expect("read terminal AI history head")
            .expect("terminal AI history head exists");
        let pinned_source = resolver
            .resolve(&fixture.context, &intent_target, terminal_source_oid)
            .await
            .expect("resolve pinned Intent dependencies");
        let unrelated_target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("unrelated-memory-write").expect("construct unrelated root"),
        );
        fixture
            .writer
            .commit(
                &fixture.context,
                &unrelated_target,
                &proposal(&unrelated_target, fixture.key_id, 1),
                None,
            )
            .await
            .expect("commit unrelated Memory revision");
        resolver
            .revalidate(&fixture.context, &intent_target, &pinned_source)
            .await
            .expect("unrelated Memory writes do not invalidate pinned Task revisions");
        let intent_outcome = runner
            .run_one(&compilers, "intent-runner", 2_000_000_100_003)
            .await
            .expect("compile Intent after all Task pins exist");
        if intent_outcome
            != (GenerationRunOutcome::Committed {
                appended: true,
                new_generation_pending: false,
            })
        {
            let job = fixture
                .database
                .query_one_raw(Statement::from_sql_and_values(
                    fixture.database.get_database_backend(),
                    "SELECT last_error_code, last_error_summary FROM memory_compile_job
                     WHERE scope_key = 'repo' AND root_kind = 'intent' AND root_id = ?",
                    [intent_id.to_string().into()],
                ))
                .await
                .expect("query failed Intent job")
                .expect("Intent job exists");
            panic!(
                "unexpected Intent outcome {intent_outcome:?}: {:?} {:?}",
                job.try_get::<Option<String>>("", "last_error_code")
                    .expect("decode Intent error code"),
                job.try_get::<Option<String>>("", "last_error_summary")
                    .expect("decode Intent error summary")
            );
        }

        let intent_head = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT live_revision_oid FROM memory_head
                 WHERE scope_key = 'repo' AND note_id = ?",
                [intent_root.note_id().to_string().into()],
            ))
            .await
            .expect("query Intent Memory head")
            .expect("Intent Memory head exists");
        let first_intent_revision: String = intent_head
            .try_get::<Option<String>>("", "live_revision_oid")
            .expect("decode Intent live revision")
            .expect("Intent has a confirmed revision");
        let first_note = parse_memory_note_v1(
            &load_note_bytes(
                history.repository_path(),
                first_intent_revision
                    .parse()
                    .expect("parse Intent revision OID"),
            )
            .expect("load Intent revision"),
        )
        .expect("parse Intent revision");
        assert_eq!(first_note.note_id, intent_root.note_id());
        assert_eq!(first_note.links.len(), 2);
        assert_eq!(
            first_note
                .episode
                .as_ref()
                .expect("Intent note has Episode payload")
                .related_task_ids
                .len(),
            2
        );
        assert!(first_note.links.iter().all(|link| {
            link.kind == super::super::domain::MemoryLinkKind::Supports
                && link.target_revision_oid.is_some()
        }));

        let revised = TaskEvent::new(actor, task_ids[0], TaskEventKind::Failed)
            .expect("construct revised terminal Task event");
        append_object(
            &history,
            "task_event",
            &revised.header().object_id().to_string(),
            &revised,
        )
        .await;
        EpisodeObserver::new(&history, fixture.database.as_ref(), &fixture.digest, "repo")
            .expect("construct terminal observer")
            .observe_terminal_events()
            .await
            .expect("observe revised Task terminal source");
        assert!(matches!(
            runner
                .run_one(&compilers, "intent-runner", 2_000_000_100_004)
                .await
                .expect("compile revised Task generation"),
            GenerationRunOutcome::Committed { appended: true, .. }
        ));
        assert!(matches!(
            runner
                .run_one(&compilers, "intent-runner", 2_000_000_100_005)
                .await
                .expect("recompile parent Intent generation"),
            GenerationRunOutcome::Committed { appended: true, .. }
        ));
        let revisions = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT COUNT(*) AS revision_count FROM memory_revision_index
                 WHERE scope_key = 'repo' AND note_id = ?",
                [intent_root.note_id().to_string().into()],
            ))
            .await
            .expect("query Intent revisions")
            .expect("Intent revision count exists");
        assert_eq!(revisions.try_get::<i64>("", "revision_count").unwrap(), 2);
        assert_eq!(
            runner
                .run_one(&compilers, "intent-runner", 2_000_000_100_006)
                .await
                .expect("repeat converged generation"),
            GenerationRunOutcome::NoWork
        );
        assert!(
            read_memory_ref_head(fixture.database.as_ref())
                .await
                .expect("read final Memory head")
                .is_some()
        );
    }
}
