//! Runtime-owned control commands shared by Code control and headless adapters.
//!
//! This module deliberately owns the mutable Goal slot and its JSONL boundary.
//! Web adapters therefore do not need a UI-specific relay merely
//! to start, inspect, or cancel a Goal.  It also keeps explicit `task.dispatch`
//! on the same `SubAgentDispatcher` gate as `/task` and exposes Code skill
//! discovery only through the A0-07 projection/registry surface.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::internal::ai::{
    agent::runtime::{SubAgentToolLoopRuntime, TaskEntryKind, TaskInvocation},
    goal::{GoalActor, GoalEvent},
    goal_session::{GoalSession, GoalSessionError, render_goal_status},
    observed_agents::{AgentKind, SkillEventProjection, SkillQuery, discover_skills},
    sandbox::{FileHistoryRuntimeContext, ToolRuntimeContext},
    session::{SessionEvent, SessionJsonlStore},
};

/// A Code-facing skill query. The search is executed against an A0-07
/// [`SkillEventProjection`]; this service intentionally has no skill store.
#[derive(Debug, Clone, Default)]
pub struct CodeSkillSearch {
    pub skill: Option<String>,
    pub provider: Option<String>,
    pub session: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

/// A curated A0-07 skill selected for activation by a Code client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSkillActivation {
    pub provider: String,
    pub name: String,
}

/// User-visible Goal control errors shared by headless and Code control adapters.
#[derive(Debug, thiserror::Error)]
pub enum GoalControlError {
    #[error("an active Goal already exists in this session")]
    AlreadyActive,
    #[error("no active Goal in this session — start one with goal.start")]
    NotActive,
    #[error("Goal objective failed validation: {0}")]
    InvalidObjective(String),
    #[error("failed to persist Goal event: {0}")]
    Persistence(String),
    #[error("Goal state error: {0}")]
    State(String),
}

/// Owns the mutable execution-control state for one Code session.
///
/// `goal_store` is optional only for in-memory test adapters. Production
/// headless construction always supplies it, so successful Goal mutations are
/// appended as `SessionEvent::Goal` before their rendered result is returned.
#[derive(Clone)]
pub struct ExecutionControlService {
    session_id: String,
    goal_store: Option<SessionJsonlStore>,
    /// Session JSONL root used to attach file-history batches on
    /// `task.dispatch` (matching the historical `/task` workflow). Derived from
    /// [`SessionJsonlStore::session_root`] when a goal store is supplied.
    session_root: Option<PathBuf>,
    goal_session: Arc<Mutex<Option<GoalSession>>>,
    subagent_runtime: Option<SubAgentToolLoopRuntime>,
}

impl ExecutionControlService {
    /// Construct the service and fold any existing Goal JSONL envelopes so a
    /// resumed headless session has the same active Goal snapshot as any Code client.
    pub fn new(
        session_id: impl Into<String>,
        goal_store: Option<SessionJsonlStore>,
        subagent_runtime: Option<SubAgentToolLoopRuntime>,
    ) -> Result<Self> {
        let session_root = goal_store
            .as_ref()
            .map(|store| store.session_root().to_path_buf());
        let resumed = goal_store
            .as_ref()
            .map(Self::replay_goal_session)
            .transpose()?
            .flatten();
        Ok(Self {
            session_id: session_id.into(),
            goal_store,
            session_root,
            goal_session: Arc::new(Mutex::new(resumed)),
            subagent_runtime,
        })
    }

    pub async fn goal_start(&self, objective: String) -> Result<String, GoalControlError> {
        let mut slot = self.goal_session.lock().await;
        if slot.as_ref().is_some_and(|session| !session.is_terminal()) {
            return Err(GoalControlError::AlreadyActive);
        }
        let actor = GoalActor::User { id: None };
        let session = GoalSession::create(
            self.session_id.clone(),
            self.session_id.clone(),
            objective,
            actor,
        )
        .map_err(map_goal_error)?;
        self.persist_goal_events(session.events())?;
        let rendered = render_goal_status(session.state());
        *slot = Some(session);
        Ok(rendered)
    }

    pub async fn goal_status(&self) -> Result<String, GoalControlError> {
        let slot = self.goal_session.lock().await;
        slot.as_ref()
            .map(|session| render_goal_status(session.state()))
            .ok_or(GoalControlError::NotActive)
    }

    pub async fn goal_cancel(&self, reason: String) -> Result<String, GoalControlError> {
        let mut slot = self.goal_session.lock().await;
        let Some(session) = slot.as_mut() else {
            return Err(GoalControlError::NotActive);
        };
        let prior_session = session.clone();
        let previous_event_count = session.events().len();
        let outcome = session
            .cancel(reason, GoalActor::User { id: None })
            .map_err(map_goal_error)?;
        if let Err(error) = self.persist_goal_events(&session.events()[previous_event_count..]) {
            // Do not expose an in-memory cancellation that cannot be replayed
            // after resume. Restore the pre-mutation projection and leave the
            // Goal active for a retry.
            *session = prior_session;
            return Err(error);
        }
        let rendered = render_goal_status(&outcome.state);
        // Cancelled Goals are terminal. Clear the active slot so a following
        // goal.start has identical behavior across headless and Code control paths.
        *slot = None;
        Ok(rendered)
    }

    /// Dispatch a user-requested child through the normal dispatcher gates.
    /// The permission-ask bypass is intentionally narrow: the human selected
    /// the agent explicitly; budgets, concurrency, depth, safety, and child
    /// runtime checks remain dispatcher-owned.
    pub async fn task_dispatch(&self, agent: String, prompt: String) -> Result<String> {
        let agent = agent.trim();
        let prompt = prompt.trim();
        if agent.is_empty() {
            return Err(anyhow!("agent must not be empty"));
        }
        if prompt.is_empty() {
            return Err(anyhow!("prompt must not be empty"));
        }
        let runtime = self.subagent_runtime.as_ref().ok_or_else(|| {
            anyhow!("task.dispatch requires code.multi_agent.enabled = true in .libra/agents.toml")
        })?;
        let invocation = TaskInvocation {
            description: task_description_from_prompt(prompt),
            prompt: prompt.to_string(),
            subagent_type: agent.to_string(),
            task_id: None,
        };
        let task_id = Uuid::new_v4();
        let live_runtime_context = user_task_runtime_context(
            runtime.runtime_context.as_ref(),
            self.session_root.as_ref(),
            task_id,
        );
        let context =
            runtime.dispatch_context(format!("user-task:{task_id}"), live_runtime_context);
        let result = runtime
            .dispatcher
            .dispatch(
                context,
                invocation,
                TaskEntryKind::UserInitiated {
                    bypass_permission_ask: true,
                },
            )
            .await
            .map_err(|error| anyhow!("task.dispatch failed: {error}"))?;
        Ok(format!(
            "Task `{}` completed with `{}` on {}/{} ({} step(s)).\n\n{}",
            result.task_id,
            result.agent_name,
            result.provider_id,
            result.model_id,
            result.steps_used,
            result.final_text.trim(),
        ))
    }

    /// Query only the A0-07 read-time projection; callers retain ownership of
    /// populating it from checkpoint metadata. No second Code skill index is
    /// constructed or persisted here.
    pub fn skill_search<'a>(
        &self,
        projection: &'a SkillEventProjection,
        search: &CodeSkillSearch,
    ) -> Vec<&'a crate::internal::ai::observed_agents::IndexedSkillEvent> {
        projection.search(&SkillQuery {
            skill: search.skill.clone(),
            provider: search.provider.clone(),
            session: search.session.clone(),
            since: search.since.clone(),
            until: search.until.clone(),
        })
    }

    /// Validate an activation against the A0-07 curated registry. Activation
    /// has no separate persistence: the Code UI adapter queues the validated
    /// activation for the next plain turn's provider input (DF-07), and the
    /// invoked provider emits the next `SkillEvent`, which A0-07 projects
    /// from checkpoint metadata.
    pub fn skill_activate(&self, activation: &CodeSkillActivation) -> Result<()> {
        let kind = AgentKind::from_cli_slug(&activation.provider).ok_or_else(|| {
            anyhow!(
                "unknown skill provider '{}'; use an A0-07 agent slug",
                activation.provider
            )
        })?;
        if discover_skills(kind)
            .iter()
            .any(|skill| skill.name == activation.name)
        {
            Ok(())
        } else {
            Err(anyhow!(
                "skill '{}' is not discoverable for provider '{}'",
                activation.name,
                activation.provider
            ))
        }
    }

    fn persist_goal_events(
        &self,
        events: &[crate::internal::ai::goal::GoalEventEnvelope],
    ) -> Result<(), GoalControlError> {
        let Some(store) = &self.goal_store else {
            return Ok(());
        };
        for event in events {
            store
                .append(&SessionEvent::goal(event.clone()))
                .map_err(|error| GoalControlError::Persistence(error.to_string()))?;
        }
        Ok(())
    }

    fn replay_goal_session(store: &SessionJsonlStore) -> Result<Option<GoalSession>> {
        let envelopes = store
            .load_events()
            .context("failed to load Goal events for Code session resume")?
            .into_iter()
            .filter_map(|event| match event {
                SessionEvent::Goal(envelope) => Some(envelope),
                _ => None,
            })
            .collect::<Vec<_>>();
        if envelopes.is_empty() {
            return Ok(None);
        }
        // A session may contain multiple Goal lifecycles (cancel A, start B).
        // `from_replay` rejects cross-goal envelopes, so resume must select the
        // latest Created goal and replay only that goal_id's stream.
        let Some(latest_goal_id) = envelopes
            .iter()
            .filter_map(|envelope| match &envelope.event {
                GoalEvent::Created(_) => Some(envelope.goal_id),
                _ => None,
            })
            .next_back()
        else {
            return Ok(None);
        };
        let filtered = envelopes
            .into_iter()
            .filter(|envelope| envelope.goal_id == latest_goal_id)
            .collect::<Vec<_>>();
        GoalSession::from_replay(filtered)
            .map(|(session, _)| session)
            .ok_or_else(|| anyhow!("Goal session JSONL replay failed: first Goal event is invalid"))
            .map(Some)
    }
}

/// Build the live runtime context for a headless/user-initiated
/// `task.dispatch`, mirroring the historical interactive `/task` file-history
/// attachment.
///
/// When `session_root` is set, clones the stored parent context (or a
/// default) and attaches a fresh file-history batch so child
/// `apply_patch` calls can record undo preimages (S2-INV-06).
fn user_task_runtime_context(
    runtime_context: Option<&ToolRuntimeContext>,
    session_root: Option<&PathBuf>,
    task_id: Uuid,
) -> Option<ToolRuntimeContext> {
    let root = session_root?;
    let mut ctx = runtime_context.cloned().unwrap_or_default();
    ctx.file_history = Some(FileHistoryRuntimeContext {
        session_root: root.clone(),
        batch_id: format!("user-task-{task_id}"),
    });
    Some(ctx)
}

fn map_goal_error(error: GoalSessionError) -> GoalControlError {
    match error {
        GoalSessionError::NotActive => GoalControlError::NotActive,
        GoalSessionError::InvalidObjective { source } => {
            GoalControlError::InvalidObjective(source.to_string())
        }
        GoalSessionError::InternalApply { detail } => GoalControlError::State(detail),
    }
}

fn task_description_from_prompt(prompt: &str) -> String {
    const MAX_DESCRIPTION_CHARS: usize = 96;
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();
    if first_line.chars().count() <= MAX_DESCRIPTION_CHARS {
        return first_line.to_string();
    }
    let mut description = first_line
        .chars()
        .take(MAX_DESCRIPTION_CHARS.saturating_sub(3))
        .collect::<String>();
    description.push_str("...");
    description
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn goal_controls_persist_and_resume_through_session_event_jsonl() {
        let temp = tempfile::tempdir().expect("temporary session root");
        let store = SessionJsonlStore::new(temp.path().join("session"));
        let service = ExecutionControlService::new("session-1", Some(store.clone()), None)
            .expect("control service");

        let started = service
            .goal_start("make the headless runtime resumable".to_string())
            .await
            .expect("goal start");
        assert!(started.contains("Active"));
        assert_eq!(
            store
                .load_events()
                .expect("events")
                .into_iter()
                .filter(|event| matches!(event, SessionEvent::Goal(_)))
                .count(),
            1
        );

        let resumed = ExecutionControlService::new("session-1", Some(store.clone()), None)
            .expect("resume control service");
        assert!(
            resumed
                .goal_status()
                .await
                .expect("resumed status")
                .contains("Active")
        );
        resumed
            .goal_cancel("user cancelled".to_string())
            .await
            .expect("goal cancel");
        assert_eq!(
            store
                .load_events()
                .expect("events")
                .into_iter()
                .filter(|event| matches!(event, SessionEvent::Goal(_)))
                .count(),
            2
        );
    }

    /// After cancel Goal A and start Goal B, resume must reconstruct B
    /// (latest Created), not fail by feeding A's envelopes into
    /// `from_replay` ahead of B.
    #[tokio::test]
    async fn resume_selects_latest_created_goal_after_cancel_and_restart() {
        let temp = tempfile::tempdir().expect("temporary session root");
        let store = SessionJsonlStore::new(temp.path().join("session"));
        let service = ExecutionControlService::new("session-1", Some(store.clone()), None)
            .expect("control service");

        service
            .goal_start("goal A objective that should not resume".to_string())
            .await
            .expect("start goal A");
        service
            .goal_cancel("switch goals".to_string())
            .await
            .expect("cancel goal A");
        service
            .goal_start("goal B is the active successor".to_string())
            .await
            .expect("start goal B");

        let resumed = ExecutionControlService::new("session-1", Some(store.clone()), None)
            .expect("resume after multi-goal JSONL");
        let status = resumed
            .goal_status()
            .await
            .expect("resumed status must reflect active Goal B");
        assert!(
            status.contains("Active"),
            "expected active Goal B after resume, got: {status}"
        );
        assert!(
            status.contains("goal B is the active successor"),
            "resume must reconstruct Goal B's objective, got: {status}"
        );
        assert!(
            !status.contains("goal A objective"),
            "resume must not surface cancelled Goal A: {status}"
        );
    }

    #[test]
    fn user_task_runtime_context_attaches_file_history_when_session_root_set() {
        let root = PathBuf::from("/tmp/session-root-for-file-history");
        let task_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("uuid");
        let ctx = user_task_runtime_context(None, Some(&root), task_id)
            .expect("live context when session_root is set");
        let file_history = ctx
            .file_history
            .expect("file_history must be attached for task.dispatch");
        assert_eq!(file_history.session_root, root);
        assert_eq!(
            file_history.batch_id,
            format!("user-task-{task_id}"),
            "batch id must match the historical user-task-{{uuid}} convention"
        );
    }

    #[test]
    fn user_task_runtime_context_is_none_without_session_root() {
        assert!(user_task_runtime_context(None, None, Uuid::new_v4()).is_none());
    }
}
