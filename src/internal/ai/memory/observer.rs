use std::{collections::BTreeMap, path::Path};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        intent_event::{IntentEvent, IntentEventKind},
        task_event::{TaskEvent, TaskEventKind},
    },
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::Value;
use thiserror::Error;

use super::{
    domain::{EpisodeRoot, EpisodeRootKind, MemoryEventAction},
    job_sql::{
        ObservationBatch, ObservationBatchOutcome, ObservedRoot, load_observer_cursor,
        load_terminal_job_source, record_observation_batch,
    },
    job_state::CompileJobKey,
    policy::REPO_EPISODE_POLICY_VERSION,
    store::read_memory_ref_head,
    tree::load_history_delta_bounded,
};
use crate::internal::ai::{
    history::{AI_REF, HistoryManager, PinnedHistoryAppend},
    keyed_digest::RepositoryKeyedDigest,
};

const MAX_OBSERVER_COMMITS: usize = 2_048;
const MAX_OBSERVER_TREE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_OBSERVER_BLOB_BYTES: u64 = 512 * 1024;
const MAX_INTENT_TASKS: usize = 128;
const MAX_TASK_SCAN: usize = 4_096;
const TASK_OBJECT_TYPE: &str = "task";
const TASK_EVENT_OBJECT_TYPE: &str = "task_event";
const INTENT_EVENT_OBJECT_TYPE: &str = "intent_event";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeObserverOutcome {
    NoHead,
    UpToDate,
    Advanced { observed_roots: usize },
}

/// Persistent terminal-event observer for the canonical AI history.
///
/// Scanning, parsing, and fingerprint derivation happen against immutable Git
/// objects. Only the final job upserts and cursor advance share a short SQLite
/// write transaction in `record_observation_batch`.
pub(crate) struct EpisodeObserver<'a> {
    history: &'a HistoryManager,
    database: &'a DatabaseConnection,
    digest: &'a RepositoryKeyedDigest,
    scope_key: &'a str,
}

impl<'a> EpisodeObserver<'a> {
    pub(crate) fn new(
        history: &'a HistoryManager,
        database: &'a DatabaseConnection,
        digest: &'a RepositoryKeyedDigest,
        scope_key: &'a str,
    ) -> Result<Self, EpisodeObserverError> {
        if history.ref_name() != AI_REF || scope_key.is_empty() {
            return Err(EpisodeObserverError::new(
                EpisodeObserverErrorKind::InvalidConfiguration,
            ));
        }
        Ok(Self {
            history,
            database,
            digest,
            scope_key,
        })
    }

    pub(crate) async fn observe_terminal_events(
        &self,
    ) -> Result<EpisodeObserverOutcome, EpisodeObserverError> {
        let Some(head) = self
            .history
            .resolve_history_head()
            .await
            .map_err(|_| EpisodeObserverError::history())?
        else {
            return Ok(EpisodeObserverOutcome::NoHead);
        };
        let cursor = load_observer_cursor(self.database, self.scope_key, AI_REF)
            .await
            .map_err(|_| EpisodeObserverError::job())?;
        if cursor == Some(head) {
            return Ok(EpisodeObserverOutcome::UpToDate);
        }
        let delta = self
            .history
            .scan_append_delta(
                head,
                cursor,
                MAX_OBSERVER_COMMITS,
                MAX_OBSERVER_TREE_BYTES,
                MAX_OBSERVER_BLOB_BYTES,
            )
            .await
            .map_err(|_| EpisodeObserverError::history())?;

        let mut roots = BTreeMap::new();
        for append in delta.appends() {
            match append.object_type() {
                TASK_EVENT_OBJECT_TYPE => {
                    if let Some(root) = self.observed_task(append)? {
                        roots.insert(("task", root.root.id().to_string()), root);
                    }
                }
                INTENT_EVENT_OBJECT_TYPE => {
                    if let Some(root) = self.observed_intent(append).await? {
                        roots.insert(("intent", root.root.id().to_string()), root);
                    }
                }
                _ => {}
            }
        }
        let batch = ObservationBatch::new(
            self.scope_key,
            AI_REF,
            cursor,
            delta.head(),
            roots.into_values().map(|root| root.observed).collect(),
        )
        .map_err(|_| EpisodeObserverError::job())?;
        match record_observation_batch(self.database, batch)
            .await
            .map_err(|_| EpisodeObserverError::job())?
        {
            ObservationBatchOutcome::Recorded { observed_roots } => {
                Ok(EpisodeObserverOutcome::Advanced { observed_roots })
            }
            ObservationBatchOutcome::AlreadyRecorded => Ok(EpisodeObserverOutcome::UpToDate),
        }
    }

    fn observed_task(
        &self,
        append: &PinnedHistoryAppend,
    ) -> Result<Option<ObservedRootWithKey>, EpisodeObserverError> {
        let event = TaskEvent::from_bytes(append.bytes(), append.object_oid())
            .map_err(|_| EpisodeObserverError::corrupt_event())?;
        if event.header().object_id().to_string() != append.object_id() {
            return Err(EpisodeObserverError::corrupt_event());
        }
        if !matches!(
            event.kind(),
            TaskEventKind::Done | TaskEventKind::Failed | TaskEventKind::Cancelled
        ) {
            return Ok(None);
        }
        let root = EpisodeRoot::task(event.task_id().to_string())
            .map_err(|_| EpisodeObserverError::corrupt_event())?;
        let fingerprint_bytes = canonical_task_input(&root, append.source_commit_oid());
        let fingerprint = self
            .digest
            .source_input_fingerprint(&fingerprint_bytes)
            .map_err(|_| EpisodeObserverError::digest())?;
        Ok(Some(ObservedRootWithKey {
            observed: ObservedRoot::new(root.clone(), append.source_commit_oid(), fingerprint),
            root,
        }))
    }

    async fn observed_intent(
        &self,
        append: &PinnedHistoryAppend,
    ) -> Result<Option<ObservedRootWithKey>, EpisodeObserverError> {
        let event = IntentEvent::from_bytes(append.bytes(), append.object_oid())
            .map_err(|_| EpisodeObserverError::corrupt_event())?;
        if event.header().object_id().to_string() != append.object_id() {
            return Err(EpisodeObserverError::corrupt_event());
        }
        if !matches!(
            event.kind(),
            IntentEventKind::Completed | IntentEventKind::Cancelled
        ) {
            return Ok(None);
        }
        let root = EpisodeRoot::intent(event.intent_id().to_string())
            .map_err(|_| EpisodeObserverError::corrupt_event())?;
        let task_revisions = intent_task_revisions(
            self.history,
            self.database,
            self.scope_key,
            &root,
            append.source_commit_oid(),
        )
        .await?;
        let fingerprint_bytes =
            canonical_intent_input(&root, append.source_commit_oid(), &task_revisions);
        let fingerprint = self
            .digest
            .source_input_fingerprint(&fingerprint_bytes)
            .map_err(|_| EpisodeObserverError::digest())?;
        Ok(Some(ObservedRootWithKey {
            observed: ObservedRoot::new(root.clone(), append.source_commit_oid(), fingerprint),
            root,
        }))
    }
}

async fn intent_task_revisions(
    history: &HistoryManager,
    database: &DatabaseConnection,
    scope_key: &str,
    intent_root: &EpisodeRoot,
    source_commit_oid: ObjectHash,
) -> Result<Vec<(String, Option<String>)>, EpisodeObserverError> {
    let view = history
        .pin_history(
            source_commit_oid,
            MAX_OBSERVER_COMMITS,
            MAX_OBSERVER_TREE_BYTES,
        )
        .await
        .map_err(|_| EpisodeObserverError::history())?;
    let listing = view
        .list(TASK_OBJECT_TYPE, MAX_TASK_SCAN)
        .map_err(|_| EpisodeObserverError::history())?;
    if listing.omitted() != 0 {
        return Err(EpisodeObserverError::new(
            EpisodeObserverErrorKind::BudgetExceeded,
        ));
    }
    let mut tasks = Vec::new();
    for entry in listing.entries() {
        let blob = view
            .read_blob(entry, MAX_OBSERVER_BLOB_BYTES)
            .map_err(|_| EpisodeObserverError::history())?;
        let value: Value = serde_json::from_slice(blob.bytes())
            .map_err(|_| EpisodeObserverError::corrupt_event())?;
        if value.get("intent").and_then(Value::as_str) != Some(intent_root.id()) {
            continue;
        }
        let task_id = blob.object_id().to_string();
        let task_root = EpisodeRoot::task(task_id.clone())
            .map_err(|_| EpisodeObserverError::corrupt_event())?;
        let revision = live_revision(database, scope_key, task_root.note_id().to_string()).await?;
        tasks.push((task_id, revision));
        if tasks.len() > MAX_INTENT_TASKS {
            return Err(EpisodeObserverError::new(
                EpisodeObserverErrorKind::BudgetExceeded,
            ));
        }
    }
    tasks.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(tasks)
}

async fn live_revision(
    database: &DatabaseConnection,
    scope_key: &str,
    note_id: String,
) -> Result<Option<String>, EpisodeObserverError> {
    database
        .query_one_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT live_revision_oid FROM memory_head
             WHERE scope_key = ? AND namespace = 'default' AND note_id = ?",
            [scope_key.into(), note_id.into()],
        ))
        .await
        .map_err(|_| EpisodeObserverError::job())?
        .map(|row| {
            row.try_get("", "live_revision_oid")
                .map_err(|_| EpisodeObserverError::job())
        })
        .transpose()
        .map(Option::flatten)
}

/// Watches the authoritative Memory ref for newly confirmed Task Episodes
/// and advances the input generation of already-terminal parent Intents.
/// Intent Episode revisions are ignored, so compiler output cannot wake
/// itself recursively.
pub(crate) struct MemoryDependencyObserver<'a> {
    history: &'a HistoryManager,
    storage_path: &'a Path,
    database: &'a DatabaseConnection,
    digest: &'a RepositoryKeyedDigest,
    scope_key: &'a str,
}

impl<'a> MemoryDependencyObserver<'a> {
    pub(crate) fn new(
        history: &'a HistoryManager,
        database: &'a DatabaseConnection,
        digest: &'a RepositoryKeyedDigest,
        scope_key: &'a str,
    ) -> Result<Self, EpisodeObserverError> {
        if history.ref_name() != AI_REF || scope_key.is_empty() {
            return Err(EpisodeObserverError::new(
                EpisodeObserverErrorKind::InvalidConfiguration,
            ));
        }
        Ok(Self {
            history,
            storage_path: history.repository_path(),
            database,
            digest,
            scope_key,
        })
    }

    pub(crate) async fn observe_task_revisions(
        &self,
    ) -> Result<EpisodeObserverOutcome, EpisodeObserverError> {
        let Some(head) = read_memory_ref_head(self.database)
            .await
            .map_err(|_| EpisodeObserverError::history())?
        else {
            return Ok(EpisodeObserverOutcome::NoHead);
        };
        let cursor = load_observer_cursor(self.database, self.scope_key, "libra/memory/repo")
            .await
            .map_err(|_| EpisodeObserverError::job())?;
        if cursor == Some(head) {
            return Ok(EpisodeObserverOutcome::UpToDate);
        }
        let delta = load_history_delta_bounded(
            self.storage_path,
            head,
            cursor,
            REPO_EPISODE_POLICY_VERSION,
            MAX_OBSERVER_COMMITS,
        )
        .map_err(|_| EpisodeObserverError::history())?;

        let mut parent_intents = BTreeMap::new();
        for record in delta.records {
            if record.event.action != MemoryEventAction::Confirmed {
                continue;
            }
            let Some(note) = record.note else {
                continue;
            };
            let Some(episode) = note.episode else {
                continue;
            };
            if episode.root_kind != EpisodeRootKind::Task {
                continue;
            }
            for intent_id in episode.related_intent_ids {
                parent_intents.insert(intent_id, record.source_commit_oid);
            }
        }

        let mut roots = Vec::with_capacity(parent_intents.len());
        for intent_id in parent_intents.into_keys() {
            let root = EpisodeRoot::intent(intent_id)
                .map_err(|_| EpisodeObserverError::corrupt_event())?;
            let key = CompileJobKey::new(self.scope_key, root.clone())
                .map_err(|_| EpisodeObserverError::job())?;
            let Some(terminal_source_oid) = load_terminal_job_source(self.database, &key)
                .await
                .map_err(|_| EpisodeObserverError::job())?
            else {
                // A Task may finish before its parent Intent. The later
                // terminal Intent observation derives the complete revision
                // set, so no speculative job is created here.
                continue;
            };
            let task_revisions = intent_task_revisions(
                self.history,
                self.database,
                self.scope_key,
                &root,
                terminal_source_oid,
            )
            .await?;
            let fingerprint = self
                .digest
                .source_input_fingerprint(&canonical_intent_input(
                    &root,
                    terminal_source_oid,
                    &task_revisions,
                ))
                .map_err(|_| EpisodeObserverError::digest())?;
            roots.push(ObservedRoot::new(root, terminal_source_oid, fingerprint));
        }
        let batch = ObservationBatch::new(self.scope_key, "libra/memory/repo", cursor, head, roots)
            .map_err(|_| EpisodeObserverError::job())?;
        match record_observation_batch(self.database, batch)
            .await
            .map_err(|_| EpisodeObserverError::job())?
        {
            ObservationBatchOutcome::Recorded { observed_roots } => {
                Ok(EpisodeObserverOutcome::Advanced { observed_roots })
            }
            ObservationBatchOutcome::AlreadyRecorded => Ok(EpisodeObserverOutcome::UpToDate),
        }
    }
}

struct ObservedRootWithKey {
    root: EpisodeRoot,
    observed: ObservedRoot,
}

fn canonical_task_input(root: &EpisodeRoot, terminal_source_oid: ObjectHash) -> Vec<u8> {
    let mut bytes = b"libra-memory-task-input-v1\0".to_vec();
    push_field(&mut bytes, root.id().as_bytes());
    push_field(&mut bytes, terminal_source_oid.to_string().as_bytes());
    bytes
}

pub(super) fn canonical_intent_input(
    root: &EpisodeRoot,
    terminal_source_oid: ObjectHash,
    task_revisions: &[(String, Option<String>)],
) -> Vec<u8> {
    let mut bytes = b"libra-memory-intent-input-v1\0".to_vec();
    push_field(&mut bytes, root.id().as_bytes());
    push_field(&mut bytes, terminal_source_oid.to_string().as_bytes());
    for (task_id, revision_oid) in task_revisions {
        push_field(&mut bytes, task_id.as_bytes());
        push_field(
            &mut bytes,
            revision_oid.as_deref().unwrap_or("missing").as_bytes(),
        );
    }
    bytes
}

fn push_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeObserverErrorKind {
    InvalidConfiguration,
    History,
    CorruptEvent,
    BudgetExceeded,
    Digest,
    Job,
}

#[derive(Debug, Error)]
#[error("Memory terminal observer failed ({kind:?})")]
pub(crate) struct EpisodeObserverError {
    kind: EpisodeObserverErrorKind,
}

impl EpisodeObserverError {
    const fn new(kind: EpisodeObserverErrorKind) -> Self {
        Self { kind }
    }

    const fn history() -> Self {
        Self::new(EpisodeObserverErrorKind::History)
    }

    const fn corrupt_event() -> Self {
        Self::new(EpisodeObserverErrorKind::CorruptEvent)
    }

    const fn digest() -> Self {
        Self::new(EpisodeObserverErrorKind::Digest)
    }

    const fn job() -> Self {
        Self::new(EpisodeObserverErrorKind::Job)
    }

    pub(crate) const fn kind(&self) -> EpisodeObserverErrorKind {
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
    use uuid::Uuid;

    use super::*;
    use crate::{
        internal::{
            ai::{
                keyed_digest::RepositoryKeyedDigest,
                memory::{
                    policy::TrustedMemoryTarget,
                    writer::tests::{fixture as writer_fixture, proposal},
                },
            },
            db,
        },
        utils::{object::write_git_object, storage::local::LocalStorage},
    };

    async fn history_fixture() -> (
        tempfile::TempDir,
        HistoryManager,
        DatabaseConnection,
        RepositoryKeyedDigest,
    ) {
        let directory = tempfile::tempdir().expect("temporary observer repository");
        let repository_path = directory.path().join(".libra");
        std::fs::create_dir_all(repository_path.join("objects")).expect("create object directory");
        let database = db::create_database(&repository_path.join("libra.db").to_string_lossy())
            .await
            .expect("initialize current schema");
        let storage = Arc::new(LocalStorage::new(repository_path.join("objects")));
        let history = HistoryManager::new(storage, repository_path, Arc::new(database.clone()));
        history.init_branch().await.expect("initialize AI history");
        let digest = RepositoryKeyedDigest::for_receipt_tests(
            "repo",
            Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").expect("fixed UUIDv4"),
            [17; 32],
            "observer-test-key",
        );
        (directory, history, database, digest)
    }

    async fn append_object<T: ObjectTrait>(
        history: &HistoryManager,
        object_type: &str,
        object_id: &str,
        object: &T,
    ) {
        let bytes = object.to_data().expect("serialize history object");
        let oid = write_git_object(history.repository_path(), "blob", &bytes)
            .expect("write history object blob");
        history
            .append(object_type, object_id, oid)
            .await
            .expect("append history object");
    }

    async fn generation(
        database: &DatabaseConnection,
        root_kind: &str,
        root_id: &str,
    ) -> Option<i64> {
        database
            .query_one_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "SELECT observed_generation FROM memory_compile_job
                 WHERE scope_key = 'repo' AND root_kind = ? AND root_id = ?",
                [root_kind.into(), root_id.into()],
            ))
            .await
            .expect("query observed generation")
            .map(|row| row.try_get("", "observed_generation").unwrap())
    }

    #[tokio::test]
    async fn observer_triggers_only_terminal_task_and_intent_events() {
        let (_directory, history, database, digest) = history_fixture().await;
        let actor = ActorRef::agent("observer-test-agent").expect("test actor");
        let intent = Intent::new(actor.clone(), "implement durable memory").expect("test intent");
        let intent_id = intent.header().object_id();
        append_object(&history, "intent", &intent_id.to_string(), &intent).await;
        let mut task = Task::new(actor.clone(), "compile task episode", None).expect("test task");
        task.set_intent(Some(intent_id));
        let task_id = task.header().object_id();
        append_object(&history, "task", &task_id.to_string(), &task).await;

        let running = TaskEvent::new(actor.clone(), task_id, TaskEventKind::Running)
            .expect("running task event");
        append_object(
            &history,
            "task_event",
            &running.header().object_id().to_string(),
            &running,
        )
        .await;
        let analyzed = IntentEvent::new(actor.clone(), intent_id, IntentEventKind::Analyzed)
            .expect("analyzed intent event");
        append_object(
            &history,
            "intent_event",
            &analyzed.header().object_id().to_string(),
            &analyzed,
        )
        .await;

        let observer = EpisodeObserver::new(&history, &database, &digest, "repo")
            .expect("observer configuration validates");
        assert_eq!(
            observer
                .observe_terminal_events()
                .await
                .expect("nonterminal scan succeeds"),
            EpisodeObserverOutcome::Advanced { observed_roots: 0 }
        );
        assert_eq!(
            generation(&database, "task", &task_id.to_string()).await,
            None
        );
        assert_eq!(
            generation(&database, "intent", &intent_id.to_string()).await,
            None
        );

        let done =
            TaskEvent::new(actor.clone(), task_id, TaskEventKind::Done).expect("done task event");
        append_object(
            &history,
            "task_event",
            &done.header().object_id().to_string(),
            &done,
        )
        .await;
        assert_eq!(
            observer
                .observe_terminal_events()
                .await
                .expect("task terminal scan succeeds"),
            EpisodeObserverOutcome::Advanced { observed_roots: 1 }
        );
        assert_eq!(
            generation(&database, "task", &task_id.to_string()).await,
            Some(1)
        );
        assert_eq!(
            observer
                .observe_terminal_events()
                .await
                .expect("same head is idempotent"),
            EpisodeObserverOutcome::UpToDate
        );

        let completed = IntentEvent::new(actor, intent_id, IntentEventKind::Completed)
            .expect("completed intent event");
        append_object(
            &history,
            "intent_event",
            &completed.header().object_id().to_string(),
            &completed,
        )
        .await;
        assert_eq!(
            observer
                .observe_terminal_events()
                .await
                .expect("intent terminal scan succeeds"),
            EpisodeObserverOutcome::Advanced { observed_roots: 1 }
        );
        assert_eq!(
            generation(&database, "intent", &intent_id.to_string()).await,
            Some(1)
        );
    }

    #[tokio::test]
    async fn observer_requeues_same_task_only_for_a_new_terminal_source() {
        let (_directory, history, database, digest) = history_fixture().await;
        let actor = ActorRef::agent("observer-test-agent").expect("test actor");
        let task_id = Uuid::new_v4();
        let done =
            TaskEvent::new(actor.clone(), task_id, TaskEventKind::Done).expect("done task event");
        append_object(
            &history,
            "task_event",
            &done.header().object_id().to_string(),
            &done,
        )
        .await;
        let observer = EpisodeObserver::new(&history, &database, &digest, "repo")
            .expect("observer configuration validates");
        observer
            .observe_terminal_events()
            .await
            .expect("first terminal scan succeeds");
        assert_eq!(
            generation(&database, "task", &task_id.to_string()).await,
            Some(1)
        );

        let failed =
            TaskEvent::new(actor, task_id, TaskEventKind::Failed).expect("failed task event");
        append_object(
            &history,
            "task_event",
            &failed.header().object_id().to_string(),
            &failed,
        )
        .await;
        observer
            .observe_terminal_events()
            .await
            .expect("second terminal scan succeeds");
        assert_eq!(
            generation(&database, "task", &task_id.to_string()).await,
            Some(2)
        );
    }

    #[tokio::test]
    async fn memory_observer_requeues_terminal_parent_after_task_confirmation() {
        let fixture = writer_fixture().await;
        let storage = Arc::new(LocalStorage::new(fixture._temp.path().join("objects")));
        let history = HistoryManager::new(
            storage,
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        );
        let actor = ActorRef::agent("dependency-observer-agent").expect("test actor");
        let intent = Intent::new(actor.clone(), "summarize the completed iteration")
            .expect("construct Intent");
        let intent_id = intent.header().object_id();
        append_object(&history, "intent", &intent_id.to_string(), &intent).await;
        let mut task =
            Task::new(actor.clone(), "implement the child change", None).expect("construct Task");
        task.set_intent(Some(intent_id));
        let task_id = task.header().object_id();
        append_object(&history, "task", &task_id.to_string(), &task).await;
        let completed = IntentEvent::new(actor, intent_id, IntentEventKind::Completed)
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
            .expect("observe terminal Intent");
        assert_eq!(
            generation(fixture.database.as_ref(), "intent", &intent_id.to_string()).await,
            Some(1)
        );

        let target = TrustedMemoryTarget::episode(
            EpisodeRoot::task(task_id.to_string()).expect("construct Task Episode target"),
        );
        let mut task_episode = proposal(&target, fixture.key_id, 1);
        let episode = task_episode
            .note_mut()
            .episode
            .as_mut()
            .expect("writer test proposal carries an Episode");
        episode.root_id = task_id.to_string();
        episode.related_task_ids = vec![task_id.to_string()];
        episode.related_intent_ids = vec![intent_id.to_string()];
        fixture
            .writer
            .commit(&fixture.context, &target, &task_episode, None)
            .await
            .expect("confirm Task Episode");

        let dependency = MemoryDependencyObserver::new(
            &history,
            fixture.database.as_ref(),
            &fixture.digest,
            "repo",
        )
        .expect("construct Memory dependency observer");
        assert_eq!(
            dependency
                .observe_task_revisions()
                .await
                .expect("observe confirmed Task Episode"),
            EpisodeObserverOutcome::Advanced { observed_roots: 1 }
        );
        assert_eq!(
            generation(fixture.database.as_ref(), "intent", &intent_id.to_string()).await,
            Some(2),
            "the confirmed Task revision changes the parent Intent fingerprint"
        );
        assert_eq!(
            dependency
                .observe_task_revisions()
                .await
                .expect("repeat Memory scan is idempotent"),
            EpisodeObserverOutcome::UpToDate
        );
    }
}
