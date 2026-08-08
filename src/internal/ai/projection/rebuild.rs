//! Rebuild and materialize Libra runtime projections from immutable AI history.
//!
//! This module intentionally works from `git-internal` formal objects (`intent`,
//! `task`, `run`, events, etc.) instead of provider-specific binding artifacts.
//! The result is a provider-neutral read model that managed and generic
//! providers can feed through persisted formal objects.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use git_internal::internal::object::{
    context_frame::{ContextFrame, FrameKind},
    intent::Intent,
    intent_event::{IntentEvent, IntentEventKind},
    patchset::PatchSet,
    plan::Plan,
    plan_step_event::{PlanStepEvent, PlanStepStatus},
    run::Run,
    run_event::{RunEvent, RunEventKind},
    task::Task,
    task_event::{TaskEvent, TaskEventKind},
    types::ActorRef,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use uuid::Uuid;

use super::{
    IntentContextFrameIndexRow, IntentPlanIndexRow, IntentTaskIndexRow, LiveContextFrameRef,
    LiveContextPinKind, LiveContextSourceKind, PlanHeadRef, PlanStepTaskIndexRow, RunEventIndexRow,
    RunPatchSetIndexRow, SchedulerState, TaskRunIndexRow, ThreadId, ThreadIntentLinkReason,
    ThreadIntentRef, ThreadParticipant, ThreadParticipantRole, ThreadProjection,
};
use crate::{
    internal::{
        ai::history::HistoryManager,
        model::{
            ai_index_intent_context_frame, ai_index_intent_plan, ai_index_intent_task,
            ai_index_plan_step_task, ai_index_run_event, ai_index_run_patchset, ai_index_task_run,
            ai_live_context_window, ai_scheduler_plan_head, ai_scheduler_selected_plan,
            ai_scheduler_state,
        },
    },
    utils::{storage::Storage, storage_ext::StorageExt},
};

const LIVE_CONTEXT_WINDOW_MAX: usize = 50;
#[derive(Debug, Clone)]
pub struct MaterializedProjection {
    pub thread: ThreadProjection,
    pub scheduler: SchedulerState,
    pub intent_plan_index: Vec<IntentPlanIndexRow>,
    pub intent_task_index: Vec<IntentTaskIndexRow>,
    pub plan_step_task_index: Vec<PlanStepTaskIndexRow>,
    pub task_run_index: Vec<TaskRunIndexRow>,
    pub run_event_index: Vec<RunEventIndexRow>,
    pub run_patchset_index: Vec<RunPatchSetIndexRow>,
    pub intent_context_frame_index: Vec<IntentContextFrameIndexRow>,
}

pub struct ProjectionRebuilder<'a> {
    storage: &'a (dyn Storage + Send + Sync),
    history: &'a HistoryManager,
}

struct HistoryObjects {
    intents: Vec<Intent>,
    intent_events: Vec<IntentEvent>,
    plans: Vec<Plan>,
    tasks: Vec<Task>,
    task_events: Vec<TaskEvent>,
    runs: Vec<Run>,
    run_events: Vec<RunEvent>,
    patchsets: Vec<PatchSet>,
    plan_step_events: Vec<PlanStepEvent>,
    context_frames: Vec<ContextFrame>,
}

impl<'a> ProjectionRebuilder<'a> {
    pub fn new(storage: &'a (dyn Storage + Send + Sync), history: &'a HistoryManager) -> Self {
        Self { storage, history }
    }

    pub async fn rebuild_latest_thread(&self) -> Result<Option<MaterializedProjection>> {
        let objects = self.read_history_objects().await?;

        self.rebuild_latest_thread_from_data(&objects)
    }

    pub async fn rebuild_all_threads(&self) -> Result<Vec<MaterializedProjection>> {
        let objects = self.read_history_objects().await?;

        self.rebuild_all_threads_from_data(&objects)
    }

    pub async fn rebuild_thread(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<MaterializedProjection>> {
        let objects = self.read_history_objects().await?;

        self.rebuild_thread_from_data(&objects, thread_id)
    }

    pub async fn materialize_latest_thread(
        &self,
        db: &DatabaseConnection,
    ) -> Result<Option<MaterializedProjection>> {
        let Some(rebuild) = self.rebuild_latest_thread().await? else {
            return Ok(None);
        };

        self.materialize_rebuild(db, &rebuild).await?;
        Ok(Some(rebuild))
    }

    pub async fn materialize_thread(
        &self,
        db: &DatabaseConnection,
        thread_id: ThreadId,
    ) -> Result<Option<MaterializedProjection>> {
        let Some(rebuild) = self.rebuild_thread(thread_id).await? else {
            return Ok(None);
        };

        self.materialize_rebuild(db, &rebuild).await?;
        Ok(Some(rebuild))
    }

    async fn materialize_rebuild(
        &self,
        db: &DatabaseConnection,
        rebuild: &MaterializedProjection,
    ) -> Result<()> {
        let txn = crate::internal::db::begin_write_transaction(db)
            .await
            .context("Failed to start projection materialization transaction")?;

        if let Err(err) = self.materialize_with_conn(&txn, rebuild).await {
            if let Err(rollback_err) = txn.rollback().await {
                return Err(anyhow::Error::new(rollback_err).context(format!(
                    "Failed to rollback projection materialization after: {err:#}"
                )));
            }
            return Err(err);
        }

        txn.commit()
            .await
            .context("Failed to commit projection materialization transaction")?;
        Ok(())
    }

    fn rebuild_latest_thread_from_data(
        &self,
        objects: &HistoryObjects,
    ) -> Result<Option<MaterializedProjection>> {
        let selected = select_latest_thread(
            &objects.intents,
            &objects.plans,
            &objects.tasks,
            &objects.runs,
        );
        let Some(selection) = selected else {
            return Ok(None);
        };

        self.rebuild_selected_thread_from_data(objects, &selection)
    }

    fn rebuild_all_threads_from_data(
        &self,
        objects: &HistoryObjects,
    ) -> Result<Vec<MaterializedProjection>> {
        let selections = select_all_threads(
            &objects.intents,
            &objects.plans,
            &objects.tasks,
            &objects.runs,
        );
        let mut projections = Vec::with_capacity(selections.len());
        for selection in selections {
            if let Some(projection) = self.rebuild_selected_thread_from_data(objects, &selection)? {
                projections.push(projection);
            }
        }
        Ok(projections)
    }

    fn rebuild_thread_from_data(
        &self,
        objects: &HistoryObjects,
        thread_id: ThreadId,
    ) -> Result<Option<MaterializedProjection>> {
        let selected = select_thread_by_id(
            &objects.intents,
            &objects.plans,
            &objects.tasks,
            &objects.runs,
            thread_id,
        );
        let Some(selection) = selected else {
            return Ok(None);
        };

        self.rebuild_selected_thread_from_data(objects, &selection)
    }

    fn rebuild_selected_thread_from_data(
        &self,
        objects: &HistoryObjects,
        selection: &SelectedThread,
    ) -> Result<Option<MaterializedProjection>> {
        let intent_map: HashMap<Uuid, &Intent> = objects
            .intents
            .iter()
            .map(|intent| (intent.header().object_id(), intent))
            .collect();
        let plan_map: HashMap<Uuid, &Plan> = objects
            .plans
            .iter()
            .map(|plan| (plan.header().object_id(), plan))
            .collect();
        let task_map: HashMap<Uuid, &Task> = objects
            .tasks
            .iter()
            .map(|task| (task.header().object_id(), task))
            .collect();
        let run_map: HashMap<Uuid, &Run> = objects
            .runs
            .iter()
            .map(|run| (run.header().object_id(), run))
            .collect();

        let selected_plans: Vec<&Plan> = objects
            .plans
            .iter()
            .filter(|plan| selection.plan_ids.contains(&plan.header().object_id()))
            .collect();
        let plan_step_to_plan = build_plan_step_index(&selected_plans);

        let selected_tasks: Vec<&Task> = objects
            .tasks
            .iter()
            .filter(|task| {
                let task_id = task.header().object_id();
                selection.task_ids.contains(&task_id)
                    || task
                        .origin_step_id()
                        .is_some_and(|step_id| plan_step_to_plan.contains_key(&step_id))
            })
            .collect();
        let selected_task_ids: HashSet<Uuid> = selected_tasks
            .iter()
            .map(|task| task.header().object_id())
            .collect();

        let selected_runs: Vec<&Run> = objects
            .runs
            .iter()
            .filter(|run| selected_task_ids.contains(&run.task()))
            .collect();
        let selected_run_ids: HashSet<Uuid> = selected_runs
            .iter()
            .map(|run| run.header().object_id())
            .collect();

        let selected_patchsets: Vec<&PatchSet> = objects
            .patchsets
            .iter()
            .filter(|patchset| selected_run_ids.contains(&patchset.run()))
            .collect();

        let selected_plan_step_events: Vec<&PlanStepEvent> = objects
            .plan_step_events
            .iter()
            .filter(|event| {
                selection.plan_ids.contains(&event.plan_id())
                    || selected_run_ids.contains(&event.run_id())
            })
            .collect();

        let selected_context_frames: Vec<&ContextFrame> = objects
            .context_frames
            .iter()
            .filter(|frame| {
                frame
                    .intent_id()
                    .is_some_and(|intent_id| selection.intent_ids.contains(&intent_id))
                    || frame
                        .plan_id()
                        .is_some_and(|plan_id| selection.plan_ids.contains(&plan_id))
                    || frame
                        .run_id()
                        .is_some_and(|run_id| selected_run_ids.contains(&run_id))
            })
            .collect();
        let context_frame_map: HashMap<Uuid, &ContextFrame> = selected_context_frames
            .iter()
            .map(|frame| (frame.header().object_id(), *frame))
            .collect();

        let selected_intent_events: Vec<&IntentEvent> = objects
            .intent_events
            .iter()
            .filter(|event| selection.intent_ids.contains(&event.intent_id()))
            .collect();
        let selected_task_events: Vec<&TaskEvent> = objects
            .task_events
            .iter()
            .filter(|event| selected_task_ids.contains(&event.task_id()))
            .collect();
        let selected_run_events: Vec<&RunEvent> = objects
            .run_events
            .iter()
            .filter(|event| selected_run_ids.contains(&event.run_id()))
            .collect();

        let selected_intents: Vec<&Intent> = objects
            .intents
            .iter()
            .filter(|intent| selection.intent_ids.contains(&intent.header().object_id()))
            .collect();

        let current_intent_id = current_intent_id(&selected_intents, &selected_intent_events);
        let latest_intent_id = selected_intents
            .iter()
            .max_by_key(|intent| {
                sort_key(intent.header().created_at(), intent.header().object_id())
            })
            .map(|intent| intent.header().object_id());

        let thread = build_thread_projection(
            selection,
            &selected_intents,
            current_intent_id,
            latest_intent_id,
            &selected_tasks,
            &selected_runs,
            &selected_plans,
        )?;

        let plan_heads = build_plan_heads(&selected_plans, current_intent_id);
        let live_context_window = build_live_context_window(&selected_context_frames);
        let task_statuses = latest_task_statuses(&selected_task_events);
        let run_statuses = latest_run_statuses(&selected_run_events);
        let ready_queue = build_ready_queue(&selected_tasks, &task_statuses);
        let active_task_id = latest_active_task(&selected_tasks, &task_statuses);
        let active_run_id = latest_active_run(&selected_runs, &run_statuses);
        let active_plan_step_id = latest_active_plan_step(&selected_plan_step_events);

        let fallback_selected_plan_id =
            selected_plan_id(&selected_runs, &selected_plans, &plan_heads);
        let selected_plan_ids =
            selected_plan_refs(&selected_plans, &plan_heads, fallback_selected_plan_id);
        let selected_plan_id = selected_plan_ids
            .first()
            .map(|plan| plan.plan_id)
            .or(fallback_selected_plan_id);

        let scheduler = SchedulerState {
            thread_id: thread.thread_id,
            selected_plan_id,
            selected_plan_ids,
            current_plan_heads: plan_heads,
            active_task_id,
            active_run_id,
            live_context_window,
            metadata: Some(json!({
                "projection_source": "formal_history_rebuild_v1",
                "ready_queue": ready_queue.iter().map(Uuid::to_string).collect::<Vec<_>>(),
                "active_plan_step_id": active_plan_step_id.map(|id| id.to_string()),
                "intent_event_count": selected_intent_events.len(),
                "task_event_count": selected_task_events.len(),
                "run_event_count": selected_run_events.len(),
            })),
            updated_at: projection_updated_at(
                &selected_intents,
                &selected_plans,
                &selected_tasks,
                &selected_runs,
                &selected_patchsets,
                &selected_context_frames,
                &selected_intent_events,
                &selected_task_events,
                &selected_run_events,
                &selected_plan_step_events,
            ),
            version: 1,
        };

        let intent_plan_index = selected_plans
            .iter()
            .map(|plan| IntentPlanIndexRow {
                intent_id: plan.intent(),
                plan_id: plan.header().object_id(),
                created_at: plan.header().created_at(),
            })
            .collect::<Vec<_>>();

        let intent_task_index = selected_tasks
            .iter()
            .filter_map(|task| {
                task.intent().map(|intent_id| IntentTaskIndexRow {
                    intent_id,
                    task_id: task.header().object_id(),
                    parent_task_id: task.parent(),
                    origin_step_id: task.origin_step_id(),
                    created_at: task.header().created_at(),
                })
            })
            .collect::<Vec<_>>();

        let plan_step_task_index = selected_tasks
            .iter()
            .filter_map(|task| {
                let step_id = task.origin_step_id()?;
                let plan_id = plan_step_to_plan.get(&step_id)?;
                Some(PlanStepTaskIndexRow {
                    plan_id: *plan_id,
                    step_id,
                    task_id: task.header().object_id(),
                    created_at: task.header().created_at(),
                })
            })
            .collect::<Vec<_>>();

        let task_run_index = build_task_run_index(&selected_runs);
        let run_event_index = build_run_event_index(&selected_run_events);
        let run_patchset_index = build_run_patchset_index(&selected_patchsets);

        let intent_context_frame_index = build_intent_context_frame_index(
            &selected_intents,
            &selected_plans,
            &selected_plan_step_events,
            &selected_context_frames,
            &context_frame_map,
        );

        let _ = (intent_map, plan_map, task_map, run_map); // maintained for future explicit joins.

        Ok(Some(MaterializedProjection {
            thread,
            scheduler,
            intent_plan_index,
            intent_task_index,
            plan_step_task_index,
            task_run_index,
            run_event_index,
            run_patchset_index,
            intent_context_frame_index,
        }))
    }

    async fn read_history_objects(&self) -> Result<HistoryObjects> {
        Ok(HistoryObjects {
            intents: self.read_objects::<Intent>("intent").await?,
            intent_events: self.read_objects::<IntentEvent>("intent_event").await?,
            plans: self.read_objects::<Plan>("plan").await?,
            tasks: self.read_objects::<Task>("task").await?,
            task_events: self.read_objects::<TaskEvent>("task_event").await?,
            runs: self.read_objects::<Run>("run").await?,
            run_events: self.read_objects::<RunEvent>("run_event").await?,
            patchsets: self.read_objects::<PatchSet>("patchset").await?,
            plan_step_events: self
                .read_objects::<PlanStepEvent>("plan_step_event")
                .await?,
            context_frames: self.read_objects::<ContextFrame>("context_frame").await?,
        })
    }

    async fn materialize_with_conn<C: ConnectionTrait>(
        &self,
        db: &C,
        rebuild: &MaterializedProjection,
    ) -> Result<()> {
        match ThreadProjection::find_by_id_with_conn(db, rebuild.thread.thread_id).await? {
            Some(existing) => {
                let mut updated = rebuild.thread.clone();
                updated.created_at = existing.created_at;
                updated.owner = existing.owner;
                updated.version = existing.version + 1;
                updated.update_with_conn(db).await?;
            }
            None => {
                rebuild.thread.create_with_conn(db).await?;
            }
        }

        self.replace_scheduler_rows(db, &rebuild.scheduler).await?;
        self.replace_index_rows(db, rebuild).await?;
        Ok(())
    }

    async fn replace_scheduler_rows<C: ConnectionTrait>(
        &self,
        db: &C,
        scheduler: &SchedulerState,
    ) -> Result<()> {
        let thread_id = scheduler.thread_id.to_string();

        ai_scheduler_plan_head::Entity::delete_many()
            .filter(ai_scheduler_plan_head::Column::ThreadId.eq(thread_id.clone()))
            .exec(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to clear scheduler plan heads for thread {}",
                    scheduler.thread_id
                )
            })?;

        ai_live_context_window::Entity::delete_many()
            .filter(ai_live_context_window::Column::ThreadId.eq(thread_id.clone()))
            .exec(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to clear live context window for thread {}",
                    scheduler.thread_id
                )
            })?;

        ai_scheduler_selected_plan::Entity::delete_many()
            .filter(ai_scheduler_selected_plan::Column::ThreadId.eq(thread_id.clone()))
            .exec(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to clear selected scheduler plans for thread {}",
                    scheduler.thread_id
                )
            })?;

        ai_scheduler_state::Entity::delete_many()
            .filter(ai_scheduler_state::Column::ThreadId.eq(thread_id.clone()))
            .exec(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to clear scheduler state row for thread {}",
                    scheduler.thread_id
                )
            })?;

        ai_scheduler_state::ActiveModel {
            thread_id: Set(thread_id.clone()),
            selected_plan_id: Set(scheduler.selected_plan_id.map(|id| id.to_string())),
            active_task_id: Set(scheduler.active_task_id.map(|id| id.to_string())),
            active_run_id: Set(scheduler.active_run_id.map(|id| id.to_string())),
            metadata_json: Set(scheduler
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .context("Failed to serialize scheduler metadata")?),
            version: Set(scheduler.version),
            updated_at: Set(scheduler.updated_at.timestamp()),
        }
        .insert(db)
        .await
        .with_context(|| {
            format!(
                "Failed to insert scheduler state row for thread {}",
                scheduler.thread_id
            )
        })?;

        for plan_head in &scheduler.current_plan_heads {
            ai_scheduler_plan_head::ActiveModel {
                thread_id: Set(thread_id.clone()),
                plan_id: Set(plan_head.plan_id.to_string()),
                ordinal: Set(plan_head.ordinal),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert scheduler plan head {} for thread {}",
                    plan_head.plan_id, scheduler.thread_id
                )
            })?;
        }

        for selected_plan in &scheduler.selected_plan_ids {
            ai_scheduler_selected_plan::ActiveModel {
                thread_id: Set(thread_id.clone()),
                plan_id: Set(selected_plan.plan_id.to_string()),
                ordinal: Set(selected_plan.ordinal),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert selected scheduler plan {} for thread {}",
                    selected_plan.plan_id, scheduler.thread_id
                )
            })?;
        }

        for frame in &scheduler.live_context_window {
            ai_live_context_window::ActiveModel {
                thread_id: Set(thread_id.clone()),
                context_frame_id: Set(frame.context_frame_id.to_string()),
                position: Set(frame.position),
                source_kind: Set(source_kind_label(&frame.source_kind).to_string()),
                pin_kind: Set(frame
                    .pin_kind
                    .as_ref()
                    .map(pin_kind_label)
                    .map(str::to_string)),
                inserted_at: Set(frame.inserted_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert live context frame {} for thread {}",
                    frame.context_frame_id, scheduler.thread_id
                )
            })?;
        }

        Ok(())
    }

    async fn replace_index_rows<C: ConnectionTrait>(
        &self,
        db: &C,
        rebuild: &MaterializedProjection,
    ) -> Result<()> {
        let intent_ids = rebuild
            .thread
            .intents
            .iter()
            .map(|intent| intent.intent_id.to_string())
            .collect::<Vec<_>>();
        let task_ids = rebuild
            .intent_task_index
            .iter()
            .map(|row| row.task_id.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let run_ids = rebuild
            .task_run_index
            .iter()
            .map(|row| row.run_id.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let plan_ids = rebuild
            .intent_plan_index
            .iter()
            .map(|row| row.plan_id.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        if !intent_ids.is_empty() {
            ai_index_intent_plan::Entity::delete_many()
                .filter(ai_index_intent_plan::Column::IntentId.is_in(intent_ids.clone()))
                .exec(db)
                .await
                .context("Failed to clear stale intent plan index rows")?;
            ai_index_intent_task::Entity::delete_many()
                .filter(ai_index_intent_task::Column::IntentId.is_in(intent_ids.clone()))
                .exec(db)
                .await
                .context("Failed to clear stale intent task index rows")?;
            ai_index_intent_context_frame::Entity::delete_many()
                .filter(ai_index_intent_context_frame::Column::IntentId.is_in(intent_ids.clone()))
                .exec(db)
                .await
                .context("Failed to clear stale intent context-frame index rows")?;
        }
        if !task_ids.is_empty() {
            ai_index_task_run::Entity::delete_many()
                .filter(ai_index_task_run::Column::TaskId.is_in(task_ids.clone()))
                .exec(db)
                .await
                .context("Failed to clear stale task run index rows")?;
        }
        if !plan_ids.is_empty() {
            ai_index_plan_step_task::Entity::delete_many()
                .filter(ai_index_plan_step_task::Column::PlanId.is_in(plan_ids.clone()))
                .exec(db)
                .await
                .context("Failed to clear stale plan step task index rows")?;
        }
        if !run_ids.is_empty() {
            ai_index_run_event::Entity::delete_many()
                .filter(ai_index_run_event::Column::RunId.is_in(run_ids.clone()))
                .exec(db)
                .await
                .context("Failed to clear stale run event index rows")?;
            ai_index_run_patchset::Entity::delete_many()
                .filter(ai_index_run_patchset::Column::RunId.is_in(run_ids.clone()))
                .exec(db)
                .await
                .context("Failed to clear stale run patchset index rows")?;
        }

        for row in &rebuild.intent_plan_index {
            ai_index_intent_plan::ActiveModel {
                intent_id: Set(row.intent_id.to_string()),
                plan_id: Set(row.plan_id.to_string()),
                created_at: Set(row.created_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert intent-plan index row {} -> {}",
                    row.intent_id, row.plan_id
                )
            })?;
        }

        for row in &rebuild.intent_task_index {
            ai_index_intent_task::ActiveModel {
                intent_id: Set(row.intent_id.to_string()),
                task_id: Set(row.task_id.to_string()),
                parent_task_id: Set(row.parent_task_id.map(|id| id.to_string())),
                origin_step_id: Set(row.origin_step_id.map(|id| id.to_string())),
                created_at: Set(row.created_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert intent-task index row {} -> {}",
                    row.intent_id, row.task_id
                )
            })?;
        }

        for row in &rebuild.plan_step_task_index {
            ai_index_plan_step_task::ActiveModel {
                plan_id: Set(row.plan_id.to_string()),
                task_id: Set(row.task_id.to_string()),
                step_id: Set(row.step_id.to_string()),
                created_at: Set(row.created_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert plan-step-task index row {}:{} -> {}",
                    row.plan_id, row.step_id, row.task_id
                )
            })?;
        }

        for row in &rebuild.task_run_index {
            ai_index_task_run::ActiveModel {
                task_id: Set(row.task_id.to_string()),
                run_id: Set(row.run_id.to_string()),
                is_latest: Set(row.is_latest),
                created_at: Set(row.created_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert task-run index row {} -> {}",
                    row.task_id, row.run_id
                )
            })?;
        }

        for row in &rebuild.run_event_index {
            ai_index_run_event::ActiveModel {
                run_id: Set(row.run_id.to_string()),
                event_id: Set(row.event_id.to_string()),
                event_kind: Set(row.event_kind.clone()),
                is_latest: Set(row.is_latest),
                created_at: Set(row.created_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert run-event index row {} -> {}",
                    row.run_id, row.event_id
                )
            })?;
        }

        for row in &rebuild.run_patchset_index {
            ai_index_run_patchset::ActiveModel {
                run_id: Set(row.run_id.to_string()),
                patchset_id: Set(row.patchset_id.to_string()),
                sequence: Set(row.sequence),
                is_latest: Set(row.is_latest),
                created_at: Set(row.created_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert run-patchset index row {} -> {}",
                    row.run_id, row.patchset_id
                )
            })?;
        }

        for row in &rebuild.intent_context_frame_index {
            ai_index_intent_context_frame::ActiveModel {
                intent_id: Set(row.intent_id.to_string()),
                context_frame_id: Set(row.context_frame_id.to_string()),
                relation_kind: Set(row.relation_kind.clone()),
                created_at: Set(row.created_at.timestamp()),
            }
            .insert(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to insert intent-context-frame index row {} -> {}",
                    row.intent_id, row.context_frame_id
                )
            })?;
        }

        Ok(())
    }

    async fn read_objects<T>(&self, object_type: &str) -> Result<Vec<T>>
    where
        T: DeserializeOwned + Send + Sync,
    {
        let objects = self
            .history
            .list_objects(object_type)
            .await
            .with_context(|| format!("Failed to list '{object_type}' objects from history"))?;

        let mut out = Vec::with_capacity(objects.len());
        for (object_id, hash) in objects {
            let value = self.storage.get_json::<T>(&hash).await.with_context(|| {
                format!("Failed to decode '{object_type}/{object_id}' from history")
            })?;
            out.push(value);
        }
        Ok(out)
    }
}

#[derive(Debug)]
struct SelectedThread {
    thread_id: ThreadId,
    intent_ids: HashSet<Uuid>,
    plan_ids: HashSet<Uuid>,
    task_ids: HashSet<Uuid>,
}

fn select_latest_thread(
    intents: &[Intent],
    plans: &[Plan],
    tasks: &[Task],
    runs: &[Run],
) -> Option<SelectedThread> {
    if !intents.is_empty() {
        let latest_intent = intents.iter().max_by_key(|intent| {
            sort_key(intent.header().created_at(), intent.header().object_id())
        })?;
        let intent_ids = connected_intent_component(intents, latest_intent.header().object_id());
        return Some(selected_thread_from_intent_ids(
            intents, plans, tasks, intent_ids,
        ));
    }

    let latest_task_id = tasks
        .iter()
        .max_by_key(|task| sort_key(task.header().created_at(), task.header().object_id()))
        .map(|task| task.header().object_id())
        .or_else(|| {
            runs.iter()
                .max_by_key(|run| sort_key(run.header().created_at(), run.header().object_id()))
                .map(|run| run.task())
        })?;

    Some(SelectedThread {
        thread_id: derive_thread_id(&[], Some(latest_task_id)),
        intent_ids: HashSet::new(),
        plan_ids: HashSet::new(),
        task_ids: HashSet::from([latest_task_id]),
    })
}

fn select_all_threads(
    intents: &[Intent],
    plans: &[Plan],
    tasks: &[Task],
    runs: &[Run],
) -> Vec<SelectedThread> {
    let mut selections = Vec::new();
    if !intents.is_empty() {
        let mut sorted_intents = intents.iter().collect::<Vec<_>>();
        sorted_intents.sort_by_key(|intent| {
            sort_key(intent.header().created_at(), intent.header().object_id())
        });

        let mut visited = HashSet::new();
        for intent in sorted_intents {
            let intent_id = intent.header().object_id();
            if visited.contains(&intent_id) {
                continue;
            }

            let component_ids = connected_intent_component(intents, intent_id);
            visited.extend(component_ids.iter().copied());
            selections.push(selected_thread_from_intent_ids(
                intents,
                plans,
                tasks,
                component_ids,
            ));
        }
    } else {
        let thread_task_ids = tasks
            .iter()
            .map(|task| task.header().object_id())
            .chain(runs.iter().map(|run| run.task()))
            .collect::<BTreeSet<_>>();
        selections.extend(thread_task_ids.into_iter().map(|task_id| SelectedThread {
            thread_id: derive_thread_id(&[], Some(task_id)),
            intent_ids: HashSet::new(),
            plan_ids: HashSet::new(),
            task_ids: HashSet::from([task_id]),
        }));
    }

    selections.sort_by_key(|selection| selection.thread_id);
    selections
}

fn select_thread_by_id(
    intents: &[Intent],
    plans: &[Plan],
    tasks: &[Task],
    runs: &[Run],
    thread_id: ThreadId,
) -> Option<SelectedThread> {
    if !intents.is_empty() {
        let mut sorted_intents = intents.iter().collect::<Vec<_>>();
        sorted_intents.sort_by_key(|intent| {
            sort_key(intent.header().created_at(), intent.header().object_id())
        });

        let mut visited = HashSet::new();
        for intent in sorted_intents {
            let intent_id = intent.header().object_id();
            if visited.contains(&intent_id) {
                continue;
            }

            let component_ids = connected_intent_component(intents, intent_id);
            visited.extend(component_ids.iter().copied());

            let selected = selected_thread_from_intent_ids(intents, plans, tasks, component_ids);
            if selected.thread_id == thread_id {
                return Some(selected);
            }
        }
    }

    if tasks
        .iter()
        .any(|task| task.header().object_id() == thread_id)
        || runs.iter().any(|run| run.task() == thread_id)
    {
        return Some(SelectedThread {
            thread_id,
            intent_ids: HashSet::new(),
            plan_ids: HashSet::new(),
            task_ids: HashSet::from([thread_id]),
        });
    }

    None
}

fn selected_thread_from_intent_ids(
    intents: &[Intent],
    plans: &[Plan],
    tasks: &[Task],
    intent_ids: HashSet<Uuid>,
) -> SelectedThread {
    let plan_ids = plans
        .iter()
        .filter(|plan| intent_ids.contains(&plan.intent()))
        .map(|plan| plan.header().object_id())
        .collect::<HashSet<_>>();
    let selected_plans = plans
        .iter()
        .filter(|plan| plan_ids.contains(&plan.header().object_id()))
        .collect::<Vec<_>>();
    let plan_step_to_plan = build_plan_step_index(&selected_plans);
    let task_ids = tasks
        .iter()
        .filter(|task| {
            task.intent()
                .is_some_and(|intent_id| intent_ids.contains(&intent_id))
                || task
                    .origin_step_id()
                    .is_some_and(|step_id| plan_step_to_plan.contains_key(&step_id))
        })
        .map(|task| task.header().object_id())
        .collect::<HashSet<_>>();
    let roots = component_roots(intents, &intent_ids);

    SelectedThread {
        thread_id: derive_thread_id(&roots, None),
        intent_ids,
        plan_ids,
        task_ids,
    }
}

fn connected_intent_component(intents: &[Intent], seed: Uuid) -> HashSet<Uuid> {
    let known_ids = intents
        .iter()
        .map(|intent| intent.header().object_id())
        .collect::<HashSet<_>>();
    let mut adjacency = HashMap::<Uuid, Vec<Uuid>>::new();
    for intent in intents {
        let intent_id = intent.header().object_id();
        adjacency.entry(intent_id).or_default();
        for parent_id in intent.parents() {
            if known_ids.contains(parent_id) {
                adjacency.entry(intent_id).or_default().push(*parent_id);
                adjacency.entry(*parent_id).or_default().push(intent_id);
            }
        }
    }

    let mut visited = HashSet::from([seed]);
    let mut queue = VecDeque::from([seed]);
    while let Some(intent_id) = queue.pop_front() {
        if let Some(neighbors) = adjacency.get(&intent_id) {
            for neighbor in neighbors {
                if visited.insert(*neighbor) {
                    queue.push_back(*neighbor);
                }
            }
        }
    }
    visited
}

fn component_roots(intents: &[Intent], component_ids: &HashSet<Uuid>) -> Vec<Uuid> {
    let mut roots = intents
        .iter()
        .filter(|intent| component_ids.contains(&intent.header().object_id()))
        .filter(|intent| {
            !intent
                .parents()
                .iter()
                .any(|parent_id| component_ids.contains(parent_id))
        })
        .map(|intent| intent.header().object_id())
        .collect::<Vec<_>>();
    roots.sort();
    roots
}

fn derive_thread_id(roots: &[Uuid], fallback_task_id: Option<Uuid>) -> Uuid {
    match roots {
        [root_id] => *root_id,
        [] => fallback_task_id.unwrap_or_else(Uuid::nil),
        many => *many.iter().min().unwrap_or(&Uuid::nil()),
    }
}

fn build_thread_projection(
    selection: &SelectedThread,
    intents: &[&Intent],
    current_intent_id: Option<Uuid>,
    latest_intent_id: Option<Uuid>,
    tasks: &[&Task],
    runs: &[&Run],
    plans: &[&Plan],
) -> Result<ThreadProjection> {
    let mut intents_sorted = intents.to_vec();
    intents_sorted
        .sort_by_key(|intent| sort_key(intent.header().created_at(), intent.header().object_id()));
    let head_ids = compute_intent_heads(&intents_sorted);
    let owner = thread_owner(&intents_sorted, tasks, runs, plans)?;
    let participants = thread_participants(&owner, &intents_sorted, tasks, runs, plans);

    let title = current_intent_id
        .and_then(|intent_id| {
            intents_sorted
                .iter()
                .find(|intent| intent.header().object_id() == intent_id)
                .map(|intent| summarize_title(intent.prompt()))
        })
        .or_else(|| {
            latest_intent_id.and_then(|intent_id| {
                intents_sorted
                    .iter()
                    .find(|intent| intent.header().object_id() == intent_id)
                    .map(|intent| summarize_title(intent.prompt()))
            })
        })
        .or_else(|| {
            tasks
                .iter()
                .max_by_key(|task| sort_key(task.header().created_at(), task.header().object_id()))
                .map(|task| summarize_title(task.title()))
        });

    let thread_intents = intents_sorted
        .iter()
        .enumerate()
        .map(|(ordinal, intent)| ThreadIntentRef {
            intent_id: intent.header().object_id(),
            ordinal: ordinal as i64,
            is_head: head_ids.contains(&intent.header().object_id()),
            linked_at: intent.header().created_at(),
            link_reason: match intent.parents().len() {
                0 => ThreadIntentLinkReason::Seed,
                1 => ThreadIntentLinkReason::Revision,
                _ => ThreadIntentLinkReason::Merge,
            },
        })
        .collect::<Vec<_>>();

    Ok(ThreadProjection {
        thread_id: selection.thread_id,
        title,
        owner,
        participants,
        current_intent_id,
        latest_intent_id,
        intents: thread_intents,
        metadata: Some(json!({
            "projection_source": "formal_history_rebuild_v1",
            "task_count": tasks.len(),
            "run_count": runs.len(),
            "plan_count": plans.len(),
        })),
        archived: false,
        created_at: projection_created_at(&intents_sorted, tasks, runs, plans),
        updated_at: projection_updated_at(
            &intents_sorted,
            plans,
            tasks,
            runs,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        ),
        version: 1,
    })
}

fn compute_intent_heads(intents: &[&Intent]) -> HashSet<Uuid> {
    let mut heads = intents
        .iter()
        .map(|intent| intent.header().object_id())
        .collect::<HashSet<_>>();
    for intent in intents {
        for parent_id in intent.parents() {
            heads.remove(parent_id);
        }
    }
    heads
}

fn current_intent_id(intents: &[&Intent], intent_events: &[&IntentEvent]) -> Option<Uuid> {
    let mut heads = compute_intent_heads(intents);
    let completed = intent_events
        .iter()
        .filter(|event| {
            matches!(
                event.kind(),
                IntentEventKind::Completed | IntentEventKind::Cancelled
            )
        })
        .map(|event| event.intent_id())
        .collect::<HashSet<_>>();
    heads.retain(|intent_id| !completed.contains(intent_id));

    intents
        .iter()
        .filter(|intent| heads.contains(&intent.header().object_id()))
        .max_by_key(|intent| sort_key(intent.header().created_at(), intent.header().object_id()))
        .map(|intent| intent.header().object_id())
        .or_else(|| {
            intents
                .iter()
                .max_by_key(|intent| {
                    sort_key(intent.header().created_at(), intent.header().object_id())
                })
                .map(|intent| intent.header().object_id())
        })
}

fn thread_owner(
    intents: &[&Intent],
    tasks: &[&Task],
    runs: &[&Run],
    plans: &[&Plan],
) -> Result<ActorRef> {
    intents
        .iter()
        .map(|intent| {
            (
                intent.header().created_at(),
                intent.header().created_by().clone(),
            )
        })
        .chain(tasks.iter().map(|task| {
            (
                task.header().created_at(),
                task.header().created_by().clone(),
            )
        }))
        .chain(
            runs.iter()
                .map(|run| (run.header().created_at(), run.header().created_by().clone())),
        )
        .chain(plans.iter().map(|plan| {
            (
                plan.header().created_at(),
                plan.header().created_by().clone(),
            )
        }))
        .min_by_key(|(at, actor)| sort_key(*at, actor.id().parse().unwrap_or(Uuid::nil())))
        .map(|(_, actor)| actor)
        .map(Ok)
        .unwrap_or_else(|| {
            ActorRef::agent("libra").map_err(|error| {
                anyhow::anyhow!("failed to construct fallback projection owner: {error}")
            })
        })
}

fn thread_participants(
    owner: &ActorRef,
    intents: &[&Intent],
    tasks: &[&Task],
    runs: &[&Run],
    plans: &[&Plan],
) -> Vec<ThreadParticipant> {
    let mut participants = BTreeMap::<(String, String), ThreadParticipant>::new();
    let all_actors = intents
        .iter()
        .map(|intent| {
            (
                intent.header().created_at(),
                intent.header().created_by().clone(),
            )
        })
        .chain(tasks.iter().map(|task| {
            (
                task.header().created_at(),
                task.header().created_by().clone(),
            )
        }))
        .chain(
            runs.iter()
                .map(|run| (run.header().created_at(), run.header().created_by().clone())),
        )
        .chain(plans.iter().map(|plan| {
            (
                plan.header().created_at(),
                plan.header().created_by().clone(),
            )
        }));

    for (joined_at, actor) in all_actors {
        let key = (actor.kind().to_string(), actor.id().to_string());
        participants
            .entry(key)
            .or_insert_with(|| ThreadParticipant {
                role: if actor.kind() == owner.kind() && actor.id() == owner.id() {
                    ThreadParticipantRole::Owner
                } else {
                    ThreadParticipantRole::Member
                },
                actor,
                joined_at,
            });
    }

    participants.into_values().collect()
}

fn summarize_title(raw: &str) -> String {
    let first_line = raw.lines().next().unwrap_or(raw).trim();
    if first_line.is_empty() {
        "AI Thread".to_string()
    } else {
        first_line.to_string()
    }
}

fn build_plan_heads(plans: &[&Plan], current_intent_id: Option<Uuid>) -> Vec<PlanHeadRef> {
    let mut filtered = plans
        .iter()
        .copied()
        .filter(|plan| current_intent_id.is_none_or(|intent_id| plan.intent() == intent_id))
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        filtered = plans.to_vec();
    }

    let mut heads = filtered
        .iter()
        .map(|plan| plan.header().object_id())
        .collect::<HashSet<_>>();
    for plan in &filtered {
        for parent_id in plan.parents() {
            heads.remove(parent_id);
        }
    }

    let mut head_ids = heads.into_iter().collect::<Vec<_>>();
    head_ids.sort_by_key(|plan_id| {
        filtered
            .iter()
            .find(|plan| plan.header().object_id() == *plan_id)
            .map(|plan| sort_key(plan.header().created_at(), plan.header().object_id()))
    });
    head_ids
        .into_iter()
        .enumerate()
        .map(|(ordinal, plan_id)| PlanHeadRef {
            plan_id,
            ordinal: ordinal as i64,
        })
        .collect()
}

fn build_live_context_window(frames: &[&ContextFrame]) -> Vec<LiveContextFrameRef> {
    let mut sorted = frames.to_vec();
    sorted.sort_by_key(|frame| sort_key(frame.header().created_at(), frame.header().object_id()));
    let selected = if sorted.len() > LIVE_CONTEXT_WINDOW_MAX {
        sorted.split_off(sorted.len() - LIVE_CONTEXT_WINDOW_MAX)
    } else {
        sorted
    };
    selected
        .into_iter()
        .enumerate()
        .map(|(position, frame)| LiveContextFrameRef {
            context_frame_id: frame.header().object_id(),
            position: position as i64,
            source_kind: frame_source_kind(frame.kind()),
            pin_kind: frame_pin_kind(frame.kind()),
            inserted_at: frame.header().created_at(),
        })
        .collect()
}

fn build_plan_step_index(plans: &[&Plan]) -> HashMap<Uuid, Uuid> {
    let mut by_step_id = HashMap::new();
    for plan in plans {
        for step in plan.steps() {
            by_step_id.insert(step.step_id(), plan.header().object_id());
        }
    }
    by_step_id
}

fn latest_task_statuses(events: &[&TaskEvent]) -> HashMap<Uuid, TaskEventKind> {
    let mut latest = HashMap::<Uuid, (DateTime<Utc>, Uuid, TaskEventKind)>::new();
    for event in events {
        let key = event.task_id();
        let candidate = (
            event.header().created_at(),
            event.header().object_id(),
            event.kind().clone(),
        );
        latest
            .entry(key)
            .and_modify(|current| {
                if (candidate.0, candidate.1) > (current.0, current.1) {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    latest
        .into_iter()
        .map(|(task_id, (_, _, kind))| (task_id, kind))
        .collect()
}

fn latest_run_statuses(events: &[&RunEvent]) -> HashMap<Uuid, RunEventKind> {
    let mut latest = HashMap::<Uuid, (DateTime<Utc>, Uuid, RunEventKind)>::new();
    for event in events {
        let key = event.run_id();
        let candidate = (
            event.header().created_at(),
            event.header().object_id(),
            event.kind().clone(),
        );
        latest
            .entry(key)
            .and_modify(|current| {
                if (candidate.0, candidate.1) > (current.0, current.1) {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    latest
        .into_iter()
        .map(|(run_id, (_, _, kind))| (run_id, kind))
        .collect()
}

fn build_ready_queue(tasks: &[&Task], task_statuses: &HashMap<Uuid, TaskEventKind>) -> Vec<Uuid> {
    let mut ready = tasks
        .iter()
        .filter(|task| {
            let status = task_statuses
                .get(&task.header().object_id())
                .cloned()
                .unwrap_or(TaskEventKind::Created);
            matches!(status, TaskEventKind::Created)
                && task.dependencies().iter().all(|dependency_id| {
                    matches!(task_statuses.get(dependency_id), Some(TaskEventKind::Done))
                })
        })
        .map(|task| (task.header().created_at(), task.header().object_id()))
        .collect::<Vec<_>>();
    ready.sort_by_key(|(created_at, task_id)| sort_key(*created_at, *task_id));
    ready.into_iter().map(|(_, task_id)| task_id).collect()
}

fn latest_active_task(
    tasks: &[&Task],
    task_statuses: &HashMap<Uuid, TaskEventKind>,
) -> Option<Uuid> {
    tasks
        .iter()
        .filter(|task| {
            matches!(
                task_statuses.get(&task.header().object_id()),
                Some(TaskEventKind::Running)
            )
        })
        .max_by_key(|task| sort_key(task.header().created_at(), task.header().object_id()))
        .map(|task| task.header().object_id())
}

fn latest_active_run(runs: &[&Run], run_statuses: &HashMap<Uuid, RunEventKind>) -> Option<Uuid> {
    runs.iter()
        .filter(|run| {
            matches!(
                run_statuses.get(&run.header().object_id()),
                Some(
                    RunEventKind::Created
                        | RunEventKind::Patching
                        | RunEventKind::Validating
                        | RunEventKind::Checkpointed
                )
            )
        })
        .max_by_key(|run| sort_key(run.header().created_at(), run.header().object_id()))
        .map(|run| run.header().object_id())
}

fn latest_active_plan_step(events: &[&PlanStepEvent]) -> Option<Uuid> {
    events
        .iter()
        .filter(|event| matches!(event.status(), PlanStepStatus::Progressing))
        .max_by_key(|event| sort_key(event.header().created_at(), event.header().object_id()))
        .map(|event| event.step_id())
}

fn selected_plan_id(runs: &[&Run], plans: &[&Plan], plan_heads: &[PlanHeadRef]) -> Option<Uuid> {
    runs.iter()
        .filter_map(|run| {
            run.plan()
                .map(|plan_id| (run.header().created_at(), plan_id))
        })
        .max_by_key(|(created_at, plan_id)| sort_key(*created_at, *plan_id))
        .map(|(_, plan_id)| plan_id)
        .or_else(|| {
            plan_heads
                .iter()
                .filter_map(|head| {
                    plans
                        .iter()
                        .find(|plan| plan.header().object_id() == head.plan_id)
                        .map(|plan| (plan.header().created_at(), plan.header().object_id()))
                })
                .max_by_key(|(created_at, plan_id)| sort_key(*created_at, *plan_id))
                .map(|(_, plan_id)| plan_id)
        })
}

fn selected_plan_refs(
    plans: &[&Plan],
    plan_heads: &[PlanHeadRef],
    fallback_selected_plan_id: Option<Uuid>,
) -> Vec<PlanHeadRef> {
    let head_ids = plan_heads
        .iter()
        .map(|head| head.plan_id)
        .collect::<HashSet<_>>();
    let mut candidates = plans
        .iter()
        .copied()
        .filter(|plan| head_ids.is_empty() || head_ids.contains(&plan.header().object_id()))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates = plans.to_vec();
    }

    let execution_plan = latest_plan_for_role(&candidates, PlanProjectionRole::Execution)
        .or_else(|| fallback_selected_plan_id.and_then(|id| find_plan_by_id(&candidates, id)));
    let test_plan = latest_plan_for_role(&candidates, PlanProjectionRole::Test);

    let mut refs = Vec::new();
    if let Some(plan) = execution_plan {
        refs.push(PlanHeadRef {
            plan_id: plan.header().object_id(),
            ordinal: 0,
        });
    }
    if let Some(plan) = test_plan
        && refs
            .iter()
            .all(|selected| selected.plan_id != plan.header().object_id())
    {
        refs.push(PlanHeadRef {
            plan_id: plan.header().object_id(),
            ordinal: refs.len() as i64,
        });
    }
    if refs.is_empty()
        && let Some(plan_id) = fallback_selected_plan_id
    {
        refs.push(PlanHeadRef {
            plan_id,
            ordinal: 0,
        });
    }

    refs
}

fn find_plan_by_id<'a>(plans: &[&'a Plan], plan_id: Uuid) -> Option<&'a Plan> {
    plans
        .iter()
        .copied()
        .find(|plan| plan.header().object_id() == plan_id)
}

fn latest_plan_for_role<'a>(plans: &[&'a Plan], role: PlanProjectionRole) -> Option<&'a Plan> {
    plans
        .iter()
        .copied()
        .filter(|plan| plan_projection_role(plan) == role)
        .max_by_key(|plan| sort_key(plan.header().created_at(), plan.header().object_id()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlanProjectionRole {
    Execution,
    Test,
}

fn plan_projection_role(plan: &Plan) -> PlanProjectionRole {
    for step in plan.steps() {
        if let Some(inputs) = step.inputs()
            && let Some(role) = inputs.get("planRole").and_then(|value| value.as_str())
        {
            return if role == "test" {
                PlanProjectionRole::Test
            } else {
                PlanProjectionRole::Execution
            };
        }
    }

    if !plan.steps().is_empty()
        && plan.steps().iter().all(|step| {
            step.inputs().is_some_and(|inputs| {
                inputs
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("gate"))
                    || inputs
                        .get("gateStage")
                        .is_some_and(|value| !value.is_null())
            })
        })
    {
        PlanProjectionRole::Test
    } else {
        PlanProjectionRole::Execution
    }
}

fn build_task_run_index(runs: &[&Run]) -> Vec<TaskRunIndexRow> {
    let mut by_task = BTreeMap::<Uuid, Vec<&Run>>::new();
    for run in runs {
        by_task.entry(run.task()).or_default().push(*run);
    }

    let mut rows = Vec::new();
    for (task_id, mut task_runs) in by_task {
        task_runs.sort_by_key(|run| sort_key(run.header().created_at(), run.header().object_id()));
        let latest_run_id = task_runs.last().map(|run| run.header().object_id());
        for run in task_runs {
            rows.push(TaskRunIndexRow {
                task_id,
                run_id: run.header().object_id(),
                is_latest: Some(run.header().object_id()) == latest_run_id,
                created_at: run.header().created_at(),
            });
        }
    }
    rows
}

fn build_run_event_index(events: &[&RunEvent]) -> Vec<RunEventIndexRow> {
    let mut by_run = BTreeMap::<Uuid, Vec<&RunEvent>>::new();
    for event in events {
        by_run.entry(event.run_id()).or_default().push(*event);
    }

    let mut rows = Vec::new();
    for (run_id, mut run_events) in by_run {
        run_events
            .sort_by_key(|event| sort_key(event.header().created_at(), event.header().object_id()));
        let latest_event_id = run_events.last().map(|event| event.header().object_id());
        for event in run_events {
            rows.push(RunEventIndexRow {
                run_id,
                event_id: event.header().object_id(),
                event_kind: run_event_kind_label(event.kind()).to_string(),
                is_latest: Some(event.header().object_id()) == latest_event_id,
                created_at: event.header().created_at(),
            });
        }
    }
    rows
}

fn build_run_patchset_index(patchsets: &[&PatchSet]) -> Vec<RunPatchSetIndexRow> {
    let mut by_run = BTreeMap::<Uuid, Vec<&PatchSet>>::new();
    for patchset in patchsets {
        by_run.entry(patchset.run()).or_default().push(*patchset);
    }

    let mut rows = Vec::new();
    for (run_id, mut run_patchsets) in by_run {
        run_patchsets.sort_by_key(|patchset| {
            (
                patchset.sequence(),
                sort_key(
                    patchset.header().created_at(),
                    patchset.header().object_id(),
                ),
            )
        });
        let latest_patchset_id = run_patchsets
            .last()
            .map(|patchset| patchset.header().object_id());
        for patchset in run_patchsets {
            rows.push(RunPatchSetIndexRow {
                run_id,
                patchset_id: patchset.header().object_id(),
                sequence: i64::from(patchset.sequence()),
                is_latest: Some(patchset.header().object_id()) == latest_patchset_id,
                created_at: patchset.header().created_at(),
            });
        }
    }
    rows
}

fn build_intent_context_frame_index(
    intents: &[&Intent],
    plans: &[&Plan],
    plan_step_events: &[&PlanStepEvent],
    context_frames: &[&ContextFrame],
    context_frame_map: &HashMap<Uuid, &ContextFrame>,
) -> Vec<IntentContextFrameIndexRow> {
    let mut rows = Vec::new();
    let mut seen = HashSet::<(Uuid, Uuid, &'static str)>::new();
    let plan_intent = plans
        .iter()
        .map(|plan| (plan.header().object_id(), plan.intent()))
        .collect::<HashMap<_, _>>();

    for intent in intents {
        for frame_id in intent.analysis_context_frames() {
            if let Some(frame) = context_frame_map.get(frame_id)
                && seen.insert((intent.header().object_id(), *frame_id, "intent_analysis"))
            {
                rows.push(IntentContextFrameIndexRow {
                    intent_id: intent.header().object_id(),
                    context_frame_id: *frame_id,
                    relation_kind: "intent_analysis".to_string(),
                    created_at: frame.header().created_at(),
                });
            }
        }
    }

    for plan in plans {
        for frame_id in plan.context_frames() {
            if let Some(frame) = context_frame_map.get(frame_id)
                && seen.insert((plan.intent(), *frame_id, "planning"))
            {
                rows.push(IntentContextFrameIndexRow {
                    intent_id: plan.intent(),
                    context_frame_id: *frame_id,
                    relation_kind: "planning".to_string(),
                    created_at: frame.header().created_at(),
                });
            }
        }
    }

    for frame in context_frames {
        let frame_id = frame.header().object_id();
        let intent_id = frame.intent_id().or_else(|| {
            frame
                .plan_id()
                .and_then(|plan_id| plan_intent.get(&plan_id).copied())
        });
        let Some(intent_id) = intent_id else {
            continue;
        };
        let relation_kind = relation_kind_for_context_frame(frame);
        if seen.insert((intent_id, frame_id, relation_kind)) {
            rows.push(IntentContextFrameIndexRow {
                intent_id,
                context_frame_id: frame_id,
                relation_kind: relation_kind.to_string(),
                created_at: frame.header().created_at(),
            });
        }
    }

    for event in plan_step_events {
        let Some(intent_id) = plan_intent.get(&event.plan_id()).copied() else {
            continue;
        };
        for frame_id in event
            .consumed_frames()
            .iter()
            .chain(event.produced_frames().iter())
        {
            if let Some(frame) = context_frame_map.get(frame_id)
                && seen.insert((intent_id, *frame_id, "execution"))
            {
                rows.push(IntentContextFrameIndexRow {
                    intent_id,
                    context_frame_id: *frame_id,
                    relation_kind: "execution".to_string(),
                    created_at: frame.header().created_at(),
                });
            }
        }
    }

    rows
}

fn relation_kind_for_context_frame(frame: &ContextFrame) -> &'static str {
    match frame.kind() {
        FrameKind::IntentAnalysis => "intent_analysis",
        FrameKind::SystemState if frame.plan_id().is_some() && frame.run_id().is_none() => {
            "planning"
        }
        FrameKind::Checkpoint => "validation",
        FrameKind::ErrorRecovery => "recovery",
        _ => {
            if frame.run_id().is_some() || frame.step_id().is_some() {
                "execution"
            } else {
                "context"
            }
        }
    }
}

fn projection_created_at(
    intents: &[&Intent],
    tasks: &[&Task],
    runs: &[&Run],
    plans: &[&Plan],
) -> DateTime<Utc> {
    intents
        .iter()
        .map(|intent| intent.header().created_at())
        .chain(tasks.iter().map(|task| task.header().created_at()))
        .chain(runs.iter().map(|run| run.header().created_at()))
        .chain(plans.iter().map(|plan| plan.header().created_at()))
        .min()
        .unwrap_or_else(Utc::now)
}

#[allow(clippy::too_many_arguments)]
fn projection_updated_at(
    intents: &[&Intent],
    plans: &[&Plan],
    tasks: &[&Task],
    runs: &[&Run],
    patchsets: &[&PatchSet],
    context_frames: &[&ContextFrame],
    intent_events: &[&IntentEvent],
    task_events: &[&TaskEvent],
    run_events: &[&RunEvent],
    plan_step_events: &[&PlanStepEvent],
) -> DateTime<Utc> {
    intents
        .iter()
        .map(|intent| intent.header().created_at())
        .chain(plans.iter().map(|plan| plan.header().created_at()))
        .chain(tasks.iter().map(|task| task.header().created_at()))
        .chain(runs.iter().map(|run| run.header().created_at()))
        .chain(
            patchsets
                .iter()
                .map(|patchset| patchset.header().created_at()),
        )
        .chain(
            context_frames
                .iter()
                .map(|frame| frame.header().created_at()),
        )
        .chain(
            intent_events
                .iter()
                .map(|event| event.header().created_at()),
        )
        .chain(task_events.iter().map(|event| event.header().created_at()))
        .chain(run_events.iter().map(|event| event.header().created_at()))
        .chain(
            plan_step_events
                .iter()
                .map(|event| event.header().created_at()),
        )
        .max()
        .unwrap_or_else(Utc::now)
}

fn frame_source_kind(kind: &FrameKind) -> LiveContextSourceKind {
    match kind {
        FrameKind::IntentAnalysis => LiveContextSourceKind::IntentAnalysis,
        FrameKind::ToolCall | FrameKind::CodeChange | FrameKind::StepSummary => {
            LiveContextSourceKind::Execution
        }
        FrameKind::Checkpoint => LiveContextSourceKind::Validation,
        FrameKind::SystemState => LiveContextSourceKind::Planning,
        FrameKind::ErrorRecovery | FrameKind::Other(_) => LiveContextSourceKind::Manual,
    }
}

fn frame_pin_kind(kind: &FrameKind) -> Option<LiveContextPinKind> {
    match kind {
        FrameKind::IntentAnalysis => Some(LiveContextPinKind::Seed),
        FrameKind::Checkpoint => Some(LiveContextPinKind::Checkpoint),
        _ => None,
    }
}

fn source_kind_label(kind: &LiveContextSourceKind) -> &'static str {
    match kind {
        LiveContextSourceKind::IntentAnalysis => "intent_analysis",
        LiveContextSourceKind::Planning => "planning",
        LiveContextSourceKind::Execution => "execution",
        LiveContextSourceKind::Validation => "validation",
        LiveContextSourceKind::Manual => "manual",
    }
}

fn pin_kind_label(kind: &LiveContextPinKind) -> &'static str {
    match kind {
        LiveContextPinKind::Seed => "seed",
        LiveContextPinKind::Checkpoint => "checkpoint",
        LiveContextPinKind::Manual => "manual",
        LiveContextPinKind::System => "system",
    }
}

fn run_event_kind_label(kind: &RunEventKind) -> &'static str {
    match kind {
        RunEventKind::Created => "created",
        RunEventKind::Patching => "patching",
        RunEventKind::Validating => "validating",
        RunEventKind::Completed => "completed",
        RunEventKind::Failed => "failed",
        RunEventKind::Checkpointed => "checkpointed",
    }
}

fn sort_key(created_at: DateTime<Utc>, object_id: Uuid) -> (i64, Uuid) {
    (created_at.timestamp_millis(), object_id)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Arc};

    use git_internal::internal::object::{
        context_frame::{ContextFrame, FrameKind},
        intent::Intent,
        patchset::PatchSet,
        plan::Plan,
        plan_step_event::{PlanStepEvent, PlanStepStatus},
        run::Run,
        run_event::{RunEvent, RunEventKind},
        task::Task,
        task_event::{TaskEvent, TaskEventKind},
        types::ActorRef,
    };
    use sea_orm::{DatabaseConnection, EntityTrait};
    use serde_json::{Value, json};
    use serial_test::serial;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::ProjectionRebuilder;
    use crate::{
        internal::{
            ai::{history::HistoryManager, projection::ThreadProjection},
            db,
            model::{
                ai_index_intent_context_frame, ai_index_intent_plan, ai_index_intent_task,
                ai_index_plan_step_task, ai_index_run_event, ai_index_run_patchset,
                ai_index_task_run, ai_scheduler_selected_plan, ai_scheduler_state,
            },
        },
        utils::{storage::local::LocalStorage, storage_ext::StorageExt, test},
    };

    async fn setup_projection_history() -> (
        tempfile::TempDir,
        Arc<LocalStorage>,
        HistoryManager,
        Arc<DatabaseConnection>,
    ) {
        let dir = tempdir().expect("tempdir");
        let _guard = test::ChangeDirGuard::new(dir.path());
        test::setup_with_new_libra_in(dir.path()).await;

        let libra_dir = dir.path().join(".libra");
        let objects_dir = libra_dir.join("objects");
        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_path = libra_dir.join("libra.db");
        let db_conn = Arc::new(
            db::establish_connection(db_path.to_str().expect("db path"))
                .await
                .expect("db"),
        );
        let history = HistoryManager::new(storage.clone(), libra_dir, db_conn.clone());
        (dir, storage, history, db_conn)
    }

    #[tokio::test]
    #[serial]
    async fn rebuild_materializes_multi_intent_heads_and_ready_queue() {
        let (_dir, storage, history, db_conn) = setup_projection_history().await;
        let actor = ActorRef::human("alice").expect("actor");

        let root = Intent::new(actor.clone(), "Root intent").expect("root");
        storage
            .put_tracked(&root, &history)
            .await
            .expect("store root");

        let branch_a = Intent::new_revision_from(actor.clone(), "Branch A", &root).expect("branch");
        storage
            .put_tracked(&branch_a, &history)
            .await
            .expect("store branch a");

        let branch_b = Intent::new_revision_from(actor.clone(), "Branch B", &root).expect("branch");
        storage
            .put_tracked(&branch_b, &history)
            .await
            .expect("store branch b");

        let mut done_task = Task::new(actor.clone(), "Done task", None).expect("task");
        done_task.set_intent(Some(branch_a.header().object_id()));
        storage
            .put_tracked(&done_task, &history)
            .await
            .expect("store done task");
        let done_event = TaskEvent::new(
            actor.clone(),
            done_task.header().object_id(),
            TaskEventKind::Done,
        )
        .expect("done event");
        storage
            .put_tracked(&done_event, &history)
            .await
            .expect("store done event");

        let mut ready_task = Task::new(actor.clone(), "Ready task", None).expect("task");
        ready_task.set_intent(Some(branch_b.header().object_id()));
        ready_task.add_dependency(done_task.header().object_id());
        storage
            .put_tracked(&ready_task, &history)
            .await
            .expect("store ready task");
        let ready_event = TaskEvent::new(
            actor.clone(),
            ready_task.header().object_id(),
            TaskEventKind::Created,
        )
        .expect("ready event");
        storage
            .put_tracked(&ready_event, &history)
            .await
            .expect("store ready event");

        let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
        let rebuild = rebuilder
            .materialize_latest_thread(db_conn.as_ref())
            .await
            .expect("materialize")
            .expect("projection");

        assert_eq!(
            rebuild.thread.latest_intent_id,
            Some(branch_b.header().object_id())
        );
        assert_eq!(
            rebuild.thread.current_intent_id,
            Some(branch_b.header().object_id())
        );
        assert_eq!(rebuild.thread.intents.len(), 3);
        let head_ids = rebuild
            .thread
            .intents
            .iter()
            .filter(|intent| intent.is_head)
            .map(|intent| intent.intent_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            head_ids,
            BTreeSet::from([branch_a.header().object_id(), branch_b.header().object_id()])
        );

        let scheduler_row =
            ai_scheduler_state::Entity::find_by_id(rebuild.thread.thread_id.to_string())
                .one(db_conn.as_ref())
                .await
                .expect("scheduler query")
                .expect("scheduler row");
        let metadata: Value = serde_json::from_str(
            scheduler_row
                .metadata_json
                .as_deref()
                .expect("scheduler metadata"),
        )
        .expect("metadata json");
        assert_eq!(
            metadata["ready_queue"],
            json!([ready_task.header().object_id().to_string()])
        );

        let stored_thread =
            ThreadProjection::find_by_id(db_conn.as_ref(), rebuild.thread.thread_id)
                .await
                .expect("load thread")
                .expect("stored thread");
        assert_eq!(stored_thread.intents.len(), 3);
    }

    #[tokio::test]
    #[serial]
    async fn targeted_rebuild_materializes_only_requested_thread() {
        let (_dir, storage, history, db_conn) = setup_projection_history().await;
        let actor = ActorRef::human("alice").expect("actor");

        let first = Intent::new(actor.clone(), "First canonical thread").expect("first intent");
        storage
            .put_tracked(&first, &history)
            .await
            .expect("store first intent");
        let mut first_task = Task::new(actor.clone(), "First task", None).expect("first task");
        first_task.set_intent(Some(first.header().object_id()));
        storage
            .put_tracked(&first_task, &history)
            .await
            .expect("store first task");

        let second = Intent::new(actor.clone(), "Second canonical thread").expect("second intent");
        storage
            .put_tracked(&second, &history)
            .await
            .expect("store second intent");
        let mut second_task = Task::new(actor, "Second task", None).expect("second task");
        second_task.set_intent(Some(second.header().object_id()));
        storage
            .put_tracked(&second_task, &history)
            .await
            .expect("store second task");

        let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
        let first_rebuild = rebuilder
            .materialize_thread(db_conn.as_ref(), first.header().object_id())
            .await
            .expect("materialize first thread")
            .expect("first projection");

        assert_eq!(first_rebuild.thread.thread_id, first.header().object_id());
        assert_eq!(first_rebuild.thread.intents.len(), 1);
        assert_eq!(
            first_rebuild.thread.intents[0].intent_id,
            first.header().object_id()
        );
        assert_eq!(first_rebuild.intent_task_index.len(), 1);
        assert_eq!(
            first_rebuild.intent_task_index[0].task_id,
            first_task.header().object_id()
        );

        let second_rebuild = rebuilder
            .rebuild_thread(second.header().object_id())
            .await
            .expect("rebuild second thread")
            .expect("second projection");
        assert_eq!(second_rebuild.thread.thread_id, second.header().object_id());
        assert_eq!(second_rebuild.thread.intents.len(), 1);
        assert_eq!(second_rebuild.intent_task_index.len(), 1);
        assert_eq!(
            second_rebuild.intent_task_index[0].task_id,
            second_task.header().object_id()
        );

        let missing = rebuilder
            .rebuild_thread(Uuid::new_v4())
            .await
            .expect("rebuild missing thread");
        assert!(missing.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn rebuild_materializes_run_state_and_indexes() {
        let (_dir, storage, history, db_conn) = setup_projection_history().await;
        let actor = ActorRef::agent("projection-rebuild-test").expect("actor");

        let intent = Intent::new(actor.clone(), "Implement thread materializer").expect("intent");
        storage
            .put_tracked(&intent, &history)
            .await
            .expect("store intent");

        let mut plan = Plan::new(actor.clone(), intent.header().object_id()).expect("plan");
        let mut step = git_internal::internal::object::plan::PlanStep::new("execute");
        step.set_inputs(Some(
            json!({"planRole": "execution", "kind": "Implementation"}),
        ));
        let step_id = step.step_id();
        plan.add_step(step);
        storage
            .put_tracked(&plan, &history)
            .await
            .expect("store plan");

        let mut test_plan =
            Plan::new(actor.clone(), intent.header().object_id()).expect("test plan");
        let mut test_step = git_internal::internal::object::plan::PlanStep::new("test");
        test_step.set_inputs(Some(json!({"planRole": "test", "kind": "Gate"})));
        test_plan.add_step(test_step);
        storage
            .put_tracked(&test_plan, &history)
            .await
            .expect("store test plan");

        let mut task = Task::new(actor.clone(), "Execute materializer", None).expect("task");
        task.set_intent(Some(intent.header().object_id()));
        task.set_origin_step_id(Some(step_id));
        storage
            .put_tracked(&task, &history)
            .await
            .expect("store task");

        let run = Run::new(
            actor.clone(),
            task.header().object_id(),
            "2f4f0f7d5e3942843096a6f1f8f7d1aa0b8bc4222f4f0f7d5e3942843096a6f1",
        )
        .expect("run");
        storage
            .put_tracked(&run, &history)
            .await
            .expect("store run");

        let base_commit = "2f4f0f7d5e3942843096a6f1f8f7d1aa0b8bc4222f4f0f7d5e3942843096a6f1";
        let mut first_patchset =
            PatchSet::new(actor.clone(), run.header().object_id(), base_commit)
                .expect("first patchset");
        first_patchset.set_sequence(0);
        storage
            .put_tracked(&first_patchset, &history)
            .await
            .expect("store first patchset");
        let mut second_patchset =
            PatchSet::new(actor.clone(), run.header().object_id(), base_commit)
                .expect("second patchset");
        second_patchset.set_sequence(1);
        storage
            .put_tracked(&second_patchset, &history)
            .await
            .expect("store second patchset");

        let mut task_event = TaskEvent::new(
            actor.clone(),
            task.header().object_id(),
            TaskEventKind::Running,
        )
        .expect("task event");
        task_event.set_run_id(Some(run.header().object_id()));
        storage
            .put_tracked(&task_event, &history)
            .await
            .expect("store task event");

        let run_event = RunEvent::new(
            actor.clone(),
            run.header().object_id(),
            RunEventKind::Created,
        )
        .expect("run event");
        storage
            .put_tracked(&run_event, &history)
            .await
            .expect("store run event");

        let mut frame = ContextFrame::new(actor.clone(), FrameKind::StepSummary, "step started")
            .expect("frame");
        frame.set_intent_id(Some(intent.header().object_id()));
        frame.set_plan_id(Some(plan.header().object_id()));
        frame.set_run_id(Some(run.header().object_id()));
        storage
            .put_tracked(&frame, &history)
            .await
            .expect("store frame");

        let mut step_event = PlanStepEvent::new(
            actor,
            plan.header().object_id(),
            step_id,
            run.header().object_id(),
            PlanStepStatus::Progressing,
        )
        .expect("step event");
        step_event.set_produced_frames(vec![frame.header().object_id()]);
        storage
            .put_tracked(&step_event, &history)
            .await
            .expect("store step event");

        let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
        let rebuild = rebuilder
            .materialize_latest_thread(db_conn.as_ref())
            .await
            .expect("materialize")
            .expect("projection");

        assert_eq!(
            rebuild.scheduler.active_task_id,
            Some(task.header().object_id())
        );
        assert_eq!(
            rebuild.scheduler.active_run_id,
            Some(run.header().object_id())
        );
        assert_eq!(
            rebuild.scheduler.selected_plan_id,
            Some(plan.header().object_id())
        );
        assert_eq!(
            rebuild
                .scheduler
                .selected_plan_ids
                .iter()
                .map(|selected| selected.plan_id)
                .collect::<Vec<_>>(),
            vec![plan.header().object_id(), test_plan.header().object_id()]
        );
        assert_eq!(rebuild.scheduler.current_plan_heads.len(), 2);
        assert_eq!(rebuild.scheduler.live_context_window.len(), 1);
        assert_eq!(rebuild.intent_plan_index.len(), 2);
        assert_eq!(rebuild.intent_task_index.len(), 1);
        assert_eq!(rebuild.plan_step_task_index.len(), 1);
        assert_eq!(rebuild.task_run_index.len(), 1);
        assert_eq!(rebuild.run_event_index.len(), 1);
        assert!(rebuild.run_event_index[0].is_latest);
        assert_eq!(rebuild.run_patchset_index.len(), 2);
        let latest_patchset = rebuild
            .run_patchset_index
            .iter()
            .find(|row| row.is_latest)
            .expect("latest patchset index row");
        assert_eq!(
            latest_patchset.patchset_id,
            second_patchset.header().object_id()
        );
        assert_eq!(latest_patchset.sequence, 1);
        assert_eq!(rebuild.intent_context_frame_index.len(), 1);

        let intent_plan_rows = ai_index_intent_plan::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("intent plan rows");
        assert_eq!(intent_plan_rows.len(), 2);
        let persisted_plan_ids = intent_plan_rows
            .iter()
            .map(|row| row.plan_id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            persisted_plan_ids,
            BTreeSet::from([
                plan.header().object_id().to_string(),
                test_plan.header().object_id().to_string(),
            ])
        );

        let intent_task_rows = ai_index_intent_task::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("intent task rows");
        assert_eq!(intent_task_rows.len(), 1);
        assert_eq!(
            intent_task_rows[0].intent_id,
            intent.header().object_id().to_string()
        );
        assert_eq!(
            intent_task_rows[0].task_id,
            task.header().object_id().to_string()
        );
        assert_eq!(intent_task_rows[0].parent_task_id, None);
        assert_eq!(
            intent_task_rows[0].origin_step_id,
            Some(step_id.to_string())
        );

        let plan_step_task_rows = ai_index_plan_step_task::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("plan step task rows");
        assert_eq!(plan_step_task_rows.len(), 1);
        assert_eq!(
            plan_step_task_rows[0].plan_id,
            plan.header().object_id().to_string()
        );
        assert_eq!(plan_step_task_rows[0].step_id, step_id.to_string());
        assert_eq!(
            plan_step_task_rows[0].task_id,
            task.header().object_id().to_string()
        );

        let task_run_rows = ai_index_task_run::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("task run rows");
        assert_eq!(task_run_rows.len(), 1);
        assert!(task_run_rows[0].is_latest);
        assert_eq!(
            task_run_rows[0].run_id,
            run.header().object_id().to_string()
        );

        let run_event_rows = ai_index_run_event::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("run event rows");
        assert_eq!(run_event_rows.len(), 1);
        assert_eq!(run_event_rows[0].event_kind, "created");
        assert!(run_event_rows[0].is_latest);

        let run_patchset_rows = ai_index_run_patchset::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("run patchset rows");
        assert_eq!(run_patchset_rows.len(), 2);
        let latest_patchset_row = run_patchset_rows
            .iter()
            .find(|row| row.is_latest)
            .expect("latest persisted patchset row");
        assert_eq!(
            latest_patchset_row.patchset_id,
            second_patchset.header().object_id().to_string()
        );
        assert_eq!(latest_patchset_row.sequence, 1);

        let selected_plan_rows = ai_scheduler_selected_plan::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("selected plan rows");
        assert_eq!(selected_plan_rows.len(), 2);
        let context_frame_rows = ai_index_intent_context_frame::Entity::find()
            .all(db_conn.as_ref())
            .await
            .expect("intent context frame rows");
        assert_eq!(context_frame_rows.len(), 1);
    }
}
