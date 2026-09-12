//! Exposes SeaORM entity modules for config, reference, reflog, and object_index tables used across the internal database layer.

pub mod ai_decision_proposal;
pub mod ai_final_decision;
pub mod ai_index_intent_context_frame;
pub mod ai_index_intent_plan;
pub mod ai_index_intent_task;
pub mod ai_index_plan_step_task;
pub mod ai_index_run_event;
pub mod ai_index_run_patchset;
pub mod ai_index_task_run;
pub mod ai_live_context_window;
pub mod ai_operation_link;
pub mod ai_risk_score_breakdown;
pub mod ai_scheduler_plan_head;
pub mod ai_scheduler_selected_plan;
pub mod ai_scheduler_state;
pub mod ai_thread;
pub mod ai_thread_intent;
pub mod ai_thread_participant;
pub mod ai_thread_provider_metadata;
pub mod ai_validation_report;
pub mod change_identity;
pub mod change_predecessor;
pub mod change_revision;
pub mod config;
pub mod config_kv;
pub mod layer;
pub mod layer_path;
pub mod metadata_kv;
pub mod object_index;
pub mod object_obliteration;
pub mod operation;
pub mod operation_head;
pub mod operation_journal;
pub mod operation_parent;
pub mod reference;
pub mod reflog;
pub mod revision_ordinal;
pub mod revision_ordinal_meta;
pub mod schema_version;
pub mod source_call_log;
pub mod sparse_view;
pub mod working_dirty;
pub mod working_dirty_meta;

#[cfg(test)]
mod reference_test;
