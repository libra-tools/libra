//! DeepSeek Harness turn ingestion for repository Memory.
//!
//! The plugin submits only the accepted user goal and final assistant text.
//! This module turns that evidence into canonical AI-history objects, then
//! reuses the normal Episode compiler, admission, writer, and projection path.

use std::{collections::HashMap, sync::Arc};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        decision::{Decision, DecisionType},
        run::Run,
        task::{GoalType, Task},
        task_event::{TaskEvent, TaskEventKind},
        types::ActorRef,
    },
};
use sea_orm::{ConnectionTrait, Statement};
use thiserror::Error;
use tokio::sync::Mutex;

use super::{
    domain::EpisodeRoot,
    runtime::{MemoryRuntime, MemoryRuntimeErrorKind},
};
use crate::{
    internal::{
        ai::{
            client::CompletionClient, context_budget::ContextBudget, history::HistoryManager,
            providers::deepseek, util::normalize_commit_anchor,
        },
        config::LocalIdentityTarget,
        head::Head,
    },
    utils::{storage::local::LocalStorage, storage_ext::StorageExt, util::DATABASE},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DshEpisodeInput {
    pub(crate) session_id: String,
    pub(crate) turn: u64,
    pub(crate) goal: String,
    pub(crate) response_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DshEpisodeRecord {
    pub(crate) task_id: String,
    pub(crate) note_id: String,
    pub(crate) revision_oid: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DshEpisodeRecordErrorKind {
    Unavailable,
    InvalidInput,
    Storage,
    Generation,
}

#[derive(Debug, Error)]
#[error("DSH Episode recording failed ({kind:?})")]
pub(crate) struct DshEpisodeRecordError {
    kind: DshEpisodeRecordErrorKind,
}

impl DshEpisodeRecordError {
    const fn new(kind: DshEpisodeRecordErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> DshEpisodeRecordErrorKind {
        self.kind
    }
}

/// Deep module for one completed DSH turn.
///
/// Its interface accepts source evidence, not a caller-authored Memory note.
/// The implementation owns the full canonical generation and projection path.
pub(crate) struct DshEpisodeRecorder {
    history: Arc<HistoryManager>,
    runtime: Arc<MemoryRuntime>,
    turn_anchors: Mutex<HashMap<String, (u64, ObjectHash)>>,
}

impl DshEpisodeRecorder {
    pub(crate) async fn open(
        history: Arc<HistoryManager>,
        model_name: impl Into<String>,
    ) -> Result<Self, DshEpisodeRecordError> {
        let model_name = model_name.into();
        if model_name.trim().is_empty() {
            return Err(DshEpisodeRecordError::new(
                DshEpisodeRecordErrorKind::Unavailable,
            ));
        }
        let database_path = history.repository_path().join(DATABASE);
        let client =
            deepseek::Client::from_resolved_env(LocalIdentityTarget::ExplicitDb(&database_path))
                .await
                .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Unavailable))?;
        let model = client.completion_model(model_name.clone());
        let runtime = MemoryRuntime::open(
            Arc::clone(&history),
            model,
            model_name.clone(),
            ContextBudget::for_provider_model("deepseek", &model_name),
        )
        .await
        .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Unavailable))?;
        Ok(Self {
            history,
            runtime: Arc::new(runtime),
            turn_anchors: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    pub(super) fn from_runtime(history: Arc<HistoryManager>, runtime: Arc<MemoryRuntime>) -> Self {
        Self {
            history,
            runtime,
            turn_anchors: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn begin_turn(
        &self,
        session_id: &str,
        turn: u64,
    ) -> Result<(), DshEpisodeRecordError> {
        if turn == 0 {
            return Err(DshEpisodeRecordError::new(
                DshEpisodeRecordErrorKind::InvalidInput,
            ));
        }
        let mut anchors = self.turn_anchors.lock().await;
        if anchors
            .get(session_id)
            .is_some_and(|(recorded, _)| *recorded == turn)
        {
            return Ok(());
        }
        let commit = Head::current_commit_result_with_conn(&self.history.database_connection())
            .await
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?
            .ok_or_else(|| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        anchors.insert(session_id.to_string(), (turn, commit));
        Ok(())
    }

    pub(crate) async fn close_session(&self, session_id: &str) {
        self.turn_anchors.lock().await.remove(session_id);
    }

    pub(crate) async fn record(
        &self,
        input: DshEpisodeInput,
    ) -> Result<DshEpisodeRecord, DshEpisodeRecordError> {
        if input.session_id.trim().is_empty()
            || input.turn == 0
            || input.goal.trim().is_empty()
            || input.response_text.trim().is_empty()
        {
            return Err(DshEpisodeRecordError::new(
                DshEpisodeRecordErrorKind::InvalidInput,
            ));
        }

        let base_commit = self
            .turn_anchors
            .lock()
            .await
            .get(&input.session_id)
            .filter(|(turn, _)| *turn == input.turn)
            .map(|(_, commit)| *commit)
            .ok_or_else(|| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        let commit = Head::current_commit_result_with_conn(&self.history.database_connection())
            .await
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?
            .ok_or_else(|| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        let actor = ActorRef::agent(format!("deepseek-harness:{}", input.session_id))
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        let mut task = Task::new(
            actor.clone(),
            input.goal,
            Some(GoalType::Other("dsh".into())),
        )
        .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        task.set_description(Some(format!(
            "DeepSeek Harness turn {} completed with this final response:\n{}",
            input.turn, input.response_text
        )));
        let task_id = task.header().object_id();
        let storage = LocalStorage::new(self.history.repository_path().join("objects"));
        storage
            .put_tracked(&task, self.history.as_ref())
            .await
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?;

        let commit_anchor = normalize_commit_anchor(&commit.to_string())
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        let base_anchor = normalize_commit_anchor(&base_commit.to_string())
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        let run = Run::new(actor.clone(), task_id, &base_anchor)
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        storage
            .put_tracked(&run, self.history.as_ref())
            .await
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?;
        // This records the repository revision at capture time; no commit is created.
        let mut decision = Decision::new(
            actor.clone(),
            run.header().object_id(),
            DecisionType::Other("dsh-turn-completed".into()),
        )
        .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        decision.set_result_commit_sha(Some(
            commit_anchor
                .parse()
                .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?,
        ));
        storage
            .put_tracked(&decision, self.history.as_ref())
            .await
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?;

        let mut terminal = TaskEvent::new(actor, task_id, TaskEventKind::Done)
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::InvalidInput))?;
        terminal.set_reason(Some(input.response_text));
        storage
            .put_tracked(&terminal, self.history.as_ref())
            .await
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?;

        let root = EpisodeRoot::task(task_id.to_string())
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Generation))?;
        self.runtime
            .generate_episode(&root)
            .await
            .map_err(|error| {
                let kind = match error.kind() {
                    MemoryRuntimeErrorKind::StorageUnavailable
                    | MemoryRuntimeErrorKind::ReceiptPersistenceFailed => {
                        DshEpisodeRecordErrorKind::Storage
                    }
                    _ => DshEpisodeRecordErrorKind::Generation,
                };
                DshEpisodeRecordError::new(kind)
            })?;
        let row = self
            .history
            .database_connection()
            .query_one_raw(Statement::from_sql_and_values(
                self.history.database_connection().get_database_backend(),
                "SELECT live_revision_oid FROM memory_head
                 WHERE scope_key = 'repo' AND namespace = 'default' AND note_id = ?",
                [root.note_id().to_string().into()],
            ))
            .await
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?
            .ok_or_else(|| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Generation))?;
        let revision_oid = row
            .try_get::<String>("", "live_revision_oid")
            .map_err(|_| DshEpisodeRecordError::new(DshEpisodeRecordErrorKind::Storage))?;

        Ok(DshEpisodeRecord {
            task_id: task_id.to_string(),
            note_id: root.note_id().to_string(),
            revision_oid,
        })
    }
}
