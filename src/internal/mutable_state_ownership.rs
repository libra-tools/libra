//! plan-20260714 §C.4.1.1 (line 2246): the centralized ownership inventory
//! for MUTABLE REPOSITORY STATE — the database half of the same rule the
//! Code/Agent config registry ([`crate::internal::config_ownership`])
//! enforces for configuration surfaces.
//!
//! Every mutable table must declare `Repository | Worktree | Composite`
//! ownership, and the guard below asserts that declaration against the
//! actual SQL corpus in BOTH directions:
//!
//! - a table declared `Worktree`-owned must really carry a `worktree_id`
//!   scope column (a declaration that outran the schema fails);
//! - a table whose schema carries `worktree_id` must be declared (the
//!   "新增 mutable state 未登记则测试失败" clause — a new scoped table
//!   cannot ship unregistered).
//!
//! The registry is EXHAUSTIVE over persistent tables: a repository-owned
//! table carries no scope column to detect, so enumerating only the scoped
//! ones would let a new repository-wide table ship unclassified. Transient
//! tables that a single migration creates and drops (down-guards, rebuild
//! scratch) are excluded by an EXPLICIT list, never by pattern — a new one
//! is a reviewed decision.

/// Ownership of one mutable state table (§C.4.1.1 vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateOwner {
    /// Per-worktree rows keyed by `worktree_id` (`''` = main).
    Worktree,
    /// Repository-wide rows shared by every worktree by design.
    Repository,
    /// Repository-wide rows that additionally carry a worktree/workspace
    /// key for provenance or routing, without being per-worktree state.
    Composite,
}

pub struct MutableStateSurface {
    pub table: &'static str,
    pub owner: StateOwner,
    /// Why this ownership, in one line — the thing a future reader needs.
    pub rationale: &'static str,
}

/// The §C.4.1.1 mutable-state registry.
pub const MUTABLE_STATE_OWNERSHIP: &[MutableStateSurface] = &[
    // ── Sequencer / operation state (W1) ─────────────────────────────────
    MutableStateSurface {
        table: "sequence_state",
        owner: StateOwner::Worktree,
        rationale: "one in-progress cherry-pick/am/revert sequence per worktree",
    },
    MutableStateSurface {
        table: "rebase_state",
        owner: StateOwner::Worktree,
        rationale: "one in-progress rebase per worktree",
    },
    MutableStateSurface {
        table: "bisect_state",
        owner: StateOwner::Worktree,
        rationale: "one bisect session per worktree; reset restores only its own HEAD",
    },
    // ── Advisory / materialization state (W1) ────────────────────────────
    MutableStateSurface {
        table: "working_dirty",
        owner: StateOwner::Worktree,
        rationale: "the dirty-set cache describes ONE working tree",
    },
    MutableStateSurface {
        table: "working_dirty_meta",
        owner: StateOwner::Worktree,
        rationale: "fingerprint/HEAD/scan-lock describe ONE scope (no id=1 singleton)",
    },
    MutableStateSurface {
        table: "layer",
        owner: StateOwner::Worktree,
        rationale: "layer registration and enablement are per-worktree",
    },
    MutableStateSurface {
        table: "layer_path",
        owner: StateOwner::Worktree,
        rationale: "materialized-path ownership is per-worktree",
    },
    MutableStateSurface {
        table: "sparse_view",
        owner: StateOwner::Worktree,
        rationale: "sparse patterns and their order are per-worktree",
    },
    MutableStateSurface {
        table: "sparse_view_meta",
        owner: StateOwner::Worktree,
        rationale: "the sparse enable toggle replaced the scope-less config key",
    },
    // ── Worktree lifecycle (W3) ──────────────────────────────────────────
    MutableStateSurface {
        table: "worktree_lifecycle",
        owner: StateOwner::Worktree,
        rationale: "lifecycle rows describe one registered worktree",
    },
    MutableStateSurface {
        table: "worktree_intent_journal",
        owner: StateOwner::Worktree,
        rationale: "add/move/remove intents are journaled per target worktree",
    },
    // ── Composite: repository rows carrying a scope key ──────────────────
    MutableStateSurface {
        table: "workspace_record",
        owner: StateOwner::Composite,
        rationale: "agent workspaces bind a repository to one linked worktree + lease fence",
    },
    MutableStateSurface {
        table: "agent_session",
        owner: StateOwner::Composite,
        rationale: "captured sessions are repository-owned and record the worktree they were \
                     observed in (W4 adds the workspace scope key)",
    },
    MutableStateSurface {
        table: "agent_export_job",
        owner: StateOwner::Composite,
        rationale: "export jobs are repository-owned and carry worktree/workspace provenance (W4)",
    },
    MutableStateSurface {
        table: "agent_import_identity",
        owner: StateOwner::Composite,
        rationale: "import identities are repository-owned and carry worktree/workspace provenance (W4)",
    },
    MutableStateSurface {
        table: "agent_workspace_scope_audit",
        owner: StateOwner::Composite,
        rationale: "audit rows record the scope an adoption was attributed to",
    },
    MutableStateSurface {
        table: "operation",
        owner: StateOwner::Composite,
        rationale: "the operation log is repository-wide, but its worktree_id is a real \
                     routing key: dedup windows and `op restore` are scope-fenced (§C.9)",
    },
    MutableStateSurface {
        table: "legacy_operation",
        owner: StateOwner::Composite,
        rationale: "the active v1 operation logger remains scope-routed during the OL-02 staging window",
    },
    MutableStateSurface {
        table: "ai_operation_link",
        owner: StateOwner::Composite,
        rationale: "AI provenance links are repository-owned and optionally carry worktree/workspace scope",
    },
    MutableStateSurface {
        table: "operation_parent",
        owner: StateOwner::Repository,
        rationale: "v2 operation genealogy edges are repository-wide and keyed by operation ids",
    },
    MutableStateSurface {
        table: "operation_head",
        owner: StateOwner::Repository,
        rationale: "v2 scope heads are repository records keyed by an opaque scope key",
    },
    MutableStateSurface {
        table: "operation_journal",
        owner: StateOwner::Repository,
        rationale: "v2 recovery journal entries are repository-wide and keyed by operation ids",
    },
    MutableStateSurface {
        table: "change_identity",
        owner: StateOwner::Repository,
        rationale: "change identities are repository-wide causal projections",
    },
    MutableStateSurface {
        table: "change_revision",
        owner: StateOwner::Repository,
        rationale: "change revisions are repository-wide commit projections",
    },
    MutableStateSurface {
        table: "change_predecessor",
        owner: StateOwner::Repository,
        rationale: "change genealogy edges are repository-wide and keyed by commit ids",
    },
    MutableStateSurface {
        table: "reference",
        owner: StateOwner::Composite,
        rationale: "branches/tags/remotes are repository-shared; HEAD rows are per-worktree \
                     via worktree_id (partial unique index per scope, W0)",
    },
    MutableStateSurface {
        table: "reflog",
        owner: StateOwner::Composite,
        rationale: "branch reflogs are repository-shared; HEAD reflog is per-worktree, so \
                     enumeration and expire are keyed by (ref_name, worktree_id)",
    },
    // ── Repository-owned mutable state ──────────────────────────────────
    MutableStateSurface {
        table: "agent_audit_log",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_capture_cloud_base",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_capture_incarnation",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_checkpoint",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_checkpoint_prune_tombstone",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_coverage_claim",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_coverage_conflict",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_coverage_revision",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_import_tombstone",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_subagent_content_claim",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_subagent_content_revision",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "agent_subagent_link",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    // Bridge durable projection (plan-20260818 LB-02). Only the session table
    // carries a `worktree_id` scope column; the event/operation/checkpoint/link
    // rows are keyed by bridge_session_id and are repository-owned.
    MutableStateSurface {
        table: "agent_bridge_session",
        owner: StateOwner::Composite,
        rationale: "bridge sessions are repository-owned and record the worktree/workspace scope they were observed in (LB-02)",
    },
    MutableStateSurface {
        table: "agent_bridge_event",
        owner: StateOwner::Repository,
        rationale: "bridge events are repository-owned and keyed by bridge_session_id (LB-02)",
    },
    MutableStateSurface {
        table: "agent_bridge_operation",
        owner: StateOwner::Repository,
        rationale: "bridge operations are repository-owned and keyed by bridge_session_id (LB-02)",
    },
    MutableStateSurface {
        table: "agent_bridge_checkpoint",
        owner: StateOwner::Repository,
        rationale: "bridge checkpoints are repository-owned and keyed by bridge_session_id (LB-02)",
    },
    MutableStateSurface {
        table: "agent_bridge_link",
        owner: StateOwner::Repository,
        rationale: "bridge association links are repository-owned and keyed by bridge_session_id (LB-02)",
    },
    MutableStateSurface {
        table: "agent_usage_stats",
        owner: StateOwner::Repository,
        rationale: "agent capture/export/coverage state (repository-owned; W4 adds workspace keys)",
    },
    MutableStateSurface {
        table: "ai_decision_proposal",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_final_decision",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_index_intent_context_frame",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_index_intent_plan",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_index_intent_task",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_index_plan_step_task",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_index_run_event",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_index_run_patchset",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_index_task_run",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_live_context_window",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_risk_score_breakdown",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_scheduler_plan_head",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_scheduler_selected_plan",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_scheduler_state",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_thread",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_thread_intent",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_thread_participant",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_thread_provider_metadata",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "ai_validation_report",
        owner: StateOwner::Repository,
        rationale: "AI thread/scheduler/index/runtime-contract rows (UUID-keyed; no cross-worktree row conflicts)",
    },
    MutableStateSurface {
        table: "approved_permission",
        owner: StateOwner::Composite,
        rationale: "Always-approvals are repository-wide by trust design (§C.4.1.1); W4-07 \
                     added the worktree_id provenance scope key (audit-only)",
    },
    MutableStateSurface {
        table: "automation_log",
        owner: StateOwner::Repository,
        rationale: "approvals and append-only logs (repo-wide by design, §C.4.1.1)",
    },
    MutableStateSurface {
        table: "source_call_log",
        owner: StateOwner::Repository,
        rationale: "approvals and append-only logs (repo-wide by design, §C.4.1.1)",
    },
    MutableStateSurface {
        table: "cherry_pick_state",
        owner: StateOwner::Repository,
        rationale: "pre-2.6 legacy sequencer tables: probed main-only for legacy detection, never written",
    },
    MutableStateSurface {
        table: "revert_sequence",
        owner: StateOwner::Repository,
        rationale: "pre-2.6 legacy sequencer tables: probed main-only for legacy detection, never written",
    },
    MutableStateSurface {
        table: "schema_versions",
        owner: StateOwner::Repository,
        rationale: "migration bookkeeping for this repository's database (created by the \
                     migration runner in Rust, not by a .sql file)",
    },
    MutableStateSurface {
        table: "config",
        owner: StateOwner::Repository,
        rationale: "repository configuration and metadata",
    },
    MutableStateSurface {
        table: "config_kv",
        owner: StateOwner::Repository,
        rationale: "repository configuration and metadata",
    },
    MutableStateSurface {
        table: "metadata_kv",
        owner: StateOwner::Repository,
        rationale: "repository configuration and metadata",
    },
    MutableStateSurface {
        table: "worktree_registry_capability",
        owner: StateOwner::Repository,
        rationale: "repository configuration and metadata",
    },
    MutableStateSurface {
        table: "notes",
        owner: StateOwner::Repository,
        rationale: "object-graph side tables (repository-wide by definition)",
    },
    MutableStateSurface {
        table: "object_index",
        owner: StateOwner::Repository,
        rationale: "object-graph side tables (repository-wide by definition)",
    },
    MutableStateSurface {
        table: "object_obliteration",
        owner: StateOwner::Repository,
        rationale: "object-graph side tables (repository-wide by definition)",
    },
    MutableStateSurface {
        table: "revision_ordinal",
        owner: StateOwner::Repository,
        rationale: "object-graph side tables (repository-wide by definition)",
    },
    MutableStateSurface {
        table: "revision_ordinal_meta",
        owner: StateOwner::Repository,
        rationale: "object-graph side tables (repository-wide by definition)",
    },
    MutableStateSurface {
        table: "legacy_operation_parent",
        owner: StateOwner::Repository,
        rationale: "operation-log companions (the log itself is the Composite row above)",
    },
    MutableStateSurface {
        table: "legacy_operation_view",
        owner: StateOwner::Repository,
        rationale: "operation-log companions (the log itself is the Composite row above)",
    },
    MutableStateSurface {
        table: "legacy_operation_view_ref",
        owner: StateOwner::Repository,
        rationale: "operation-log companions (the log itself is the Composite row above)",
    },
    MutableStateSurface {
        table: "legacy_operation_view_workspace",
        owner: StateOwner::Repository,
        rationale: "operation-log companions (the log itself is the Composite row above)",
    },
    MutableStateSurface {
        table: "publish_ai_objects",
        owner: StateOwner::Repository,
        rationale: "publish worker/site state (repository-wide)",
    },
    MutableStateSurface {
        table: "publish_ai_versions",
        owner: StateOwner::Repository,
        rationale: "publish worker/site state (repository-wide)",
    },
    MutableStateSurface {
        table: "publish_files",
        owner: StateOwner::Repository,
        rationale: "publish worker/site state (repository-wide)",
    },
    MutableStateSurface {
        table: "publish_refs",
        owner: StateOwner::Repository,
        rationale: "publish worker/site state (repository-wide)",
    },
    MutableStateSurface {
        table: "publish_revisions",
        owner: StateOwner::Repository,
        rationale: "publish worker/site state (repository-wide)",
    },
    MutableStateSurface {
        table: "publish_sites",
        owner: StateOwner::Repository,
        rationale: "publish worker/site state (repository-wide)",
    },
    MutableStateSurface {
        table: "publish_sync_runs",
        owner: StateOwner::Repository,
        rationale: "publish worker/site state (repository-wide)",
    },
];

/// Tables a SINGLE migration creates and drops within its own transaction —
/// down-migration CHECK guards and rebuild scratch. They hold no state
/// between migrations, so they carry no ownership. Listed EXPLICITLY rather
/// than matched by name pattern: a new one must be a reviewed decision, not
/// something a naming convention waves through.
pub const MIGRATION_ONLY_TABLES: &[&str] = &[
    "_agent_coverage_conflict_down_guard",
    "_agent_import_identity_down_guard",
    "_agent_subagent_content_down_guard",
    "_agent_subagent_replication_down_guard",
    "_agent_usage_event_session_scope_down_guard",
    "_agent_usage_runtime_attribution_down_guard",
    "_agent_tombstone_compat_down_guard",
    "_agent_tombstone_down_guard",
    "agent_capture_scope_down_guard",
    "agent_bridge_capture_down_guard",
    "agent_bridge_link_relations_down_guard",
    "agent_usage_stats__rebuild",
    "approved_permission_provenance_down_guard",
    "bisect_state__down_guard_2026072301",
    "head_scope_unique_guard",
    "layer__down_guard_2026072303",
    "layer__legacy_rows_need_explicit_adopt_2026072303",
    "operation__down_guard_2026073003",
    "operation__down_guard_2026073004",
    "legacy_operation__staging",
    "legacy_operation_parent__staging",
    "legacy_operation_view__staging",
    "legacy_operation_view_ref__staging",
    "legacy_operation_view_workspace__staging",
    "operation_view",
    "operation_view_ref",
    "operation_view_workspace",
    "operation_scope_provenance_down_guard",
    "rebase_state__down_guard_2026072101",
    "sequence_state__down_guard_2026071901",
    "source_call_log__rebuild",
    "sparse_view__down_guard_2026072304",
    "sparse_view__legacy_needs_explicit_adopt_2026072304",
    "working_dirty__down_guard_2026072302",
    "workspace_record_down_guard",
    "worktree_lifecycle_down_guard",
    "worktree_migrate_intent_down_guard",
];

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

    use super::*;

    /// The table name following a `create table` occurrence, skipping the
    /// optional `if not exists` keywords TOKEN BY TOKEN. A literal
    /// `strip_prefix("if not exists")` breaks on irregular whitespace (a
    /// doc comment in the migration runner wraps it), and the fallout is
    /// silent: the scan reads `if` as a table name.
    fn body_after_create_table(chunk: &str) -> &str {
        let mut rest = chunk.trim_start();
        for keyword in ["if", "not", "exists"] {
            if let Some(stripped) = rest.strip_prefix(keyword) {
                rest = stripped.trim_start();
            }
        }
        rest
    }

    /// The table name at the head of [`body_after_create_table`]'s output.
    fn table_name_after(chunk: &str) -> String {
        body_after_create_table(chunk)
            .trim_start_matches(['`', '"'])
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .collect()
    }

    /// Rust sources that issue DDL against a database OTHER than the local
    /// repository one, and are therefore out of this inventory's scope.
    /// Listed explicitly so a new such file is a reviewed decision.
    const NON_REPOSITORY_DDL_SOURCES: &[&str] = &[
        // The Cloudflare D1 backup mirror: a separate remote database with
        // its own schema (`sql/publish/` + the mirror tables). Local
        // worktree scoping does not apply to it.
        "src/utils/d1_client.rs",
    ];

    /// Rust-side DDL found in PRODUCTION code: the SQL text of every string
    /// literal, plus every table built from a sea-orm entity.
    ///
    /// This walks the syn AST rather than the source text. Text stripping
    /// cannot tell an attribute from a `//` comment that mentions one, from
    /// a field attribute, or from a braced `use` item — every such mistake
    /// silently DELETES production code from the corpus, which is the exact
    /// failure mode this inventory exists to prevent. `strip_cfg_test_items`
    /// (two revisions of it) made all three mistakes.
    #[derive(Default)]
    struct RustDdl {
        /// Concatenated string-literal contents, lowercased.
        sql: String,
        /// Tables created via `create_table_from_entity(<table>::Entity)`,
        /// which carry no `CREATE TABLE` text at all.
        entities: BTreeSet<String>,
    }

    /// `<table>` from a `create_table_from_entity(<table>::Entity)` argument.
    fn entity_table_name(
        args: &syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>,
    ) -> Option<String> {
        let syn::Expr::Path(path) = args.first()? else {
            return None;
        };
        let mut segments: Vec<String> = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if segments.last().is_some_and(|last| last == "Entity") {
            segments.pop();
        }
        segments.pop().map(|name| name.to_lowercase())
    }

    impl<'ast> syn::visit::Visit<'ast> for RustDdl {
        /// Macro ARGUMENTS are opaque tokens to `syn::visit`, so DDL passed
        /// to `format!`/`assert_eq!`/… would otherwise be invisible. Parse
        /// the body as a comma-separated expression list (the shape almost
        /// every such macro has) and scan those expressions; a body that is
        /// not an expression list — `quote!`, `matches!`, a DSL — is left
        /// alone.
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if let Ok(args) = mac.parse_body_with(
                syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
            ) {
                let mut nested = RustDdl::default();
                for arg in &args {
                    syn::visit::visit_expr(&mut nested, arg);
                }
                self.sql.push_str(&nested.sql);
                self.entities.extend(nested.entities);
            }
            syn::visit::visit_macro(self, mac);
        }

        fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
            self.sql.push_str(&lit.value().to_lowercase());
            self.sql.push('\n');
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == "create_table_from_entity"
                && let Some(table) = entity_table_name(&call.args)
            {
                self.entities.insert(table);
            }
            syn::visit::visit_expr_method_call(self, call);
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = call.func.as_ref()
                && path
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "create_table_from_entity")
                && let Some(table) = entity_table_name(&call.args)
            {
                self.entities.insert(table);
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    /// Scan one Rust source for DDL, with its `#[cfg(test)]` code pruned by
    /// the shared scanner ([`crate::internal::source_scan`]).
    fn scan_rust_source(label: &str, source: &str) -> RustDdl {
        let file = crate::internal::source_scan::parse_production_file(label, source);
        let mut scanner = RustDdl::default();
        syn::visit::Visit::visit_file(&mut scanner, &file);
        scanner
    }

    /// Every table the corpus CREATEs, scoped or not — from the `sql/` tree
    /// AND from Rust-side DDL, because a table created in Rust is just as
    /// persistent (`schema_versions` is created in the migration runner and
    /// was invisible to a SQL-only scan).
    fn all_created_tables() -> BTreeSet<String> {
        let rust = rust_ddl();
        let mut created = tables_in(&rust.sql);
        created.extend(rust.entities.iter().cloned());
        created.extend(tables_in(&sql_corpus()));
        created
    }

    /// Every `CREATE TABLE` name in a lowercased SQL corpus.
    fn tables_in(sql: &str) -> BTreeSet<String> {
        let mut created = BTreeSet::new();
        for chunk in sql.split("create table").skip(1) {
            let name = table_name_after(chunk);
            if !name.is_empty() {
                created.insert(name);
            }
        }
        created
    }

    /// PRODUCTION Rust DDL across the whole `src/` tree, minus the explicit
    /// non-repository sources. `#[cfg(test)]` items are dropped by the AST
    /// walk above; whole test-only FILES carry no marker of their own (their
    /// parent declares them `#[cfg(test)] mod tests;`) and are excluded by
    /// path.
    fn rust_ddl() -> RustDdl {
        let mut found = RustDdl::default();
        // This module's own source is excluded: its registry rows name every
        // table as a string literal, which would trivially satisfy the scan.
        let excludes = {
            let mut excludes = NON_REPOSITORY_DDL_SOURCES.to_vec();
            excludes.push("src/internal/mutable_state_ownership.rs");
            excludes
        };
        for (_label, file) in crate::internal::source_scan::production_files(&excludes) {
            let mut scanner = RustDdl::default();
            syn::visit::Visit::visit_file(&mut scanner, &file);
            found.sql.push_str(&scanner.sql);
            found.entities.extend(scanner.entities);
        }
        found
    }

    /// The whole `sql/` tree, lowercased with `--` line comments stripped.
    fn sql_corpus() -> String {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut corpus = String::new();
        fn collect(dir: &Path, into: &mut String) {
            for entry in fs::read_dir(dir).expect("sql dir must be readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect(&path, into);
                } else if path.extension().is_some_and(|ext| ext == "sql") {
                    into.push_str(&fs::read_to_string(&path).expect("readable sql"));
                    into.push('\n');
                }
            }
        }
        collect(&manifest_dir.join("sql"), &mut corpus);
        corpus
            .to_lowercase()
            .lines()
            .map(|line| line.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every `CREATE TABLE` in the SQL/Rust DDL corpus whose body declares a
    /// `worktree_id` column, paired with the table name.
    fn tables_with_scope_column() -> BTreeSet<String> {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut corpus = String::new();
        fn collect(dir: &Path, into: &mut String) {
            for entry in fs::read_dir(dir).expect("sql dir must be readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect(&path, into);
                } else if path.extension().is_some_and(|ext| ext == "sql") {
                    into.push_str(&fs::read_to_string(&path).expect("readable sql"));
                    into.push('\n');
                }
            }
        }
        collect(&manifest_dir.join("sql"), &mut corpus);
        // Strip `--` line comments BEFORE parsing: this corpus documents
        // itself heavily, and a comment containing `;` or an unbalanced
        // paren would otherwise cut a column list short (one such comment —
        // "Soft FK to `ai_thread(thread_id)`; ON DELETE SET NULL …" — hid
        // `agent_session`'s scope column from an earlier version of this
        // scan).
        let lowered: String = corpus
            .to_lowercase()
            .lines()
            .map(|line| line.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let mut scoped = BTreeSet::new();
        // `legacy_operation` is created by the copy-first Rust migration
        // helper, whose final table name is assembled outside a standalone
        // SQL file. Keep that known production shape in the scope inventory;
        // the broader Rust corpus contains remote D1 schemas that are not
        // part of the local repository database.
        scoped.insert("legacy_operation".to_string());
        for chunk in lowered.split("create table").skip(1) {
            let after = body_after_create_table(chunk);
            let name = table_name_after(chunk);
            // The column list is the BALANCED paren group after the name —
            // robust against `CHECK(... IN (...))` nesting and against
            // one-line statements running into the next.
            let Some(open) = after.find('(') else {
                continue;
            };
            let mut depth = 0usize;
            let mut end = None;
            for (offset, ch) in after[open..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let body = match end {
                Some(end) => &after[open..end],
                None => continue,
            };
            if !name.is_empty() && body.contains("worktree_id") {
                scoped.insert(name);
            }
        }
        // A table can also GAIN the scope column later — `ALTER TABLE <t>
        // ADD COLUMN worktree_id` is how the capture/export family got
        // theirs, and a CREATE-only scan would miss exactly those.
        for chunk in lowered.split("alter table").skip(1) {
            let after = chunk.trim_start();
            let name: String = after
                .trim_start_matches(['`', '"'])
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            // Rust doc literals are included in the DDL corpus so generated
            // SQL is visible, but prose such as `ALTER TABLE ADD COLUMN`
            // does not name a table. Do not classify that keyword as one.
            if matches!(name.as_str(), "add" | "drop" | "rename" | "if") {
                continue;
            }
            let statement = after.split(';').next().unwrap_or("");
            if !name.is_empty()
                && statement.contains("add column")
                && statement.contains("worktree_id")
            {
                scoped.insert(name);
            }
        }
        scoped
    }

    /// The AST scanner on the shapes that broke two textual predecessors.
    ///
    /// Each case here corresponds to a REAL construct in this tree that a
    /// substring-based stripper mis-read as "a test item starts here", after
    /// which it deleted production code — and with it any DDL that code
    /// contained. The fixture asserts both directions: production DDL is
    /// kept, and genuine test-only DDL is dropped.
    #[test]
    fn rust_ddl_scanner_ignores_only_real_test_items() {
        const FIXTURE: &str = r##"
             // A doc/line comment that merely MENTIONS #[cfg(test)] — the
             // text stripper skipped to the next brace and ate this file.
             fn commented_marker() {
                 let _ = "CREATE TABLE IF NOT EXISTS `after_comment` (id TEXT)";
             }

             struct Mixed {
                 real: u8,
                 #[cfg(test)]
                 only_for_tests: u8,
                 also_real: u8,
             }

             #[cfg(test)]
             use std::collections::{BTreeMap, BTreeSet};

             fn after_braced_use() {
                 let _ = "CREATE TABLE `after_use` (id TEXT)";
             }

             #[cfg(feature = "test-provider")]
             fn feature_gated_is_production() {
                 let _ = "CREATE TABLE `after_feature_gate` (id TEXT)";
             }

             // MENTIONING `test` is not being test-only: this one is compiled
             // in production and nowhere else.
             #[cfg(not(test))]
             fn production_only() {
                 let _ = "CREATE TABLE `not_test` (id TEXT)";
             }

             // Compiled in any debug build, test or not.
             #[cfg(any(test, debug_assertions))]
             fn test_or_debug() {
                 let _ = "CREATE TABLE `any_test_or_debug` (id TEXT)";
             }

             // A NAME-VALUE predicate BEFORE `test`: the predecessor walked
             // nested meta with a callback and stopped on the first item it
             // could not read, so ordering could hide the `test` term.
             #[cfg(all(feature = "x", test))]
             fn name_value_then_test() {
                 let _ = "CREATE TABLE `name_value_then_test` (id TEXT)";
             }

             #[cfg(any(feature = "x", test))]
             fn name_value_or_test() {
                 let _ = "CREATE TABLE `any_feature_or_test` (id TEXT)";
             }

             fn attributed_statements() {
                 #[cfg(test)]
                 {
                     let _ = "CREATE TABLE `stmt_test_only` (id TEXT)";
                 }
                 #[cfg(not(test))]
                 {
                     let _ = "CREATE TABLE `stmt_production` (id TEXT)";
                 }
                 #[cfg(test)]
                 let _fixture = "CREATE TABLE `local_test_only` (id TEXT)";
                 // Macro statements in both directions. These are only
                 // meaningful because `visit_macro` scans macro ARGUMENTS —
                 // without that, neither string would be collected and the
                 // `Stmt::Macro` filter could not be mutation-tested.
                 #[cfg(all(test, unix))]
                 assert_eq!("CREATE TABLE `macro_stmt_test_only` (id TEXT)", "");
                 let _ = format!("CREATE TABLE `macro_stmt_production` (id TEXT){}", "");
             }

             fn entity_ddl() {
                 let _ = schema.create_table_from_entity(object_index::Entity);
             }

             #[cfg(all(test, unix))]
             mod tests {
                 fn fixture() {
                     let _ = "CREATE TABLE `fixture_only` (id TEXT)";
                     let _ = schema.create_table_from_entity(fixture_entity::Entity);
                 }
             }

             fn trailing_production() {
                 let _ = "CREATE TABLE `after_test_module` (id TEXT)";
             }
         "##;

        let scanned = scan_rust_source("fixture.rs", FIXTURE);
        let tables = tables_in(&scanned.sql);
        assert_eq!(
            tables.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "after_comment",
                "after_feature_gate",
                "after_test_module",
                "after_use",
                "any_feature_or_test",
                "any_test_or_debug",
                "macro_stmt_production",
                "not_test",
                "stmt_production",
            ],
            "production DDL must survive every marker-shaped construct, and \
              test-module DDL must not"
        );
        assert_eq!(
            scanned
                .entities
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["object_index"],
            "entity DDL follows the same rule"
        );
    }

    /// GROUND TRUTH for the inventory: every table a REAL repository
    /// database ends up with must be classified.
    ///
    /// The scans above parse source text, and text parsing has a blind spot
    /// for every construction path it was not taught (a `#[cfg(test)]`
    /// truncation once hid `object_index`; sea-orm entities carry no
    /// `CREATE TABLE` at all). This test asks SQLite instead: create a
    /// database through the production entry point — bootstrap schema plus
    /// every registered migration — and read `sqlite_schema` back. Nothing
    /// in that list may be unclassified.
    ///
    /// It does NOT replace the source scans: tables created lazily at
    /// runtime (`object_index`, minted by `libra cloud`) are absent from a
    /// freshly initialized database, so the two directions cover different
    /// halves and both are required.
    #[tokio::test]
    async fn materialized_schema_tables_are_all_classified() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(db_path.to_str().expect("utf-8 temp path"))
            .await
            .expect("create a repository database through the production path");

        let rows = conn
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT name FROM sqlite_schema WHERE type = 'table' \
                  AND name NOT LIKE 'sqlite_%' ORDER BY name",
            ))
            .await
            .expect("read the materialized schema");
        let materialized: BTreeSet<String> = rows
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("name column"))
            .collect();

        // Self-check: an empty read would make every assertion below vacuous.
        assert!(
            materialized.contains("schema_versions") && materialized.len() > 20,
            "the materialized schema looks empty or partial: {materialized:?}"
        );

        let declared: BTreeSet<&str> = MUTABLE_STATE_OWNERSHIP
            .iter()
            .map(|surface| surface.table)
            .collect();
        for table in &materialized {
            assert!(
                declared.contains(table.as_str())
                    || MIGRATION_ONLY_TABLES.contains(&table.as_str()),
                "a REAL repository database contains table `{table}`, which is in \
                  neither MUTABLE_STATE_OWNERSHIP nor MIGRATION_ONLY_TABLES — the \
                  source scan missed it (plan-20260714 §C.4.1.1, line 2246)"
            );
        }
    }

    /// §C.4.1.1 (plan line 2246): the mutable-state scope registration
    /// guard. BOTH directions — a `Worktree`/`Composite` declaration must be
    /// backed by a real `worktree_id` column, and every table that HAS one
    /// must be declared. A new per-worktree table cannot ship unregistered.
    #[test]
    fn mutable_state_scope_registration_guard() {
        let scoped_in_schema = tables_with_scope_column();
        // Self-check: an empty or broken scan must fail loudly rather than
        // vacuously passing the forward direction below.
        assert!(
            scoped_in_schema.contains("sequence_state"),
            "scan self-check: the known scoped table `sequence_state` was not \
              found — the SQL scan is broken, not the schema"
        );

        // Self-check for the RUST half of the corpus: `schema_versions` is
        // created only in `db/migration.rs`, so a SQL-only scan (or a Rust
        // scan that silently read nothing) loses it.
        let rust = rust_ddl();
        assert!(
            rust.sql.contains("create table"),
            "scan self-check: the Rust DDL corpus contains no CREATE TABLE —              the source walk is broken"
        );
        let created = all_created_tables();
        assert!(
            created.contains("schema_versions"),
            "scan self-check: `schema_versions` (created in Rust,              src/internal/db/migration.rs) is missing from the created-table              scan — Rust-side DDL is no longer being read"
        );
        // Asserted against the ENTITY scan alone: `object_index` also has a
        // `CREATE TABLE` in `sql/`, so checking the union would pass even
        // with the entity scan deleted.
        let entity_built = &rust.entities;
        assert!(
            entity_built.is_subset(&created),
            "`all_created_tables` dropped entity-built DDL: {:?}",
            entity_built.difference(&created).collect::<Vec<_>>()
        );
        assert!(
            entity_built.contains("object_index"),
            "scan self-check: `object_index` (built from a sea-orm ENTITY in \
              src/command/cloud.rs, with no CREATE TABLE text) is missing — \
              entity-built DDL is no longer being read"
        );
        // And the non-repository exclusion must stay honest: each listed
        // file must exist AND actually issue DDL, or the entry is stale.
        for source in NON_REPOSITORY_DDL_SOURCES {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(source);
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{source} must be readable: {error}"))
                .to_lowercase();
            assert!(
                text.contains("create table"),
                "`{source}` is excluded from the ownership inventory as                  non-repository DDL, but it no longer creates any table —                  drop the exclusion instead of carrying it"
            );
        }

        let declared_scoped: BTreeSet<&str> = MUTABLE_STATE_OWNERSHIP
            .iter()
            .filter(|surface| matches!(surface.owner, StateOwner::Worktree | StateOwner::Composite))
            .map(|surface| surface.table)
            .collect();

        // FORWARD: a table with a scope column must be declared.
        for table in &scoped_in_schema {
            assert!(
                declared_scoped.contains(table.as_str()),
                "table `{table}` carries a `worktree_id` scope column but is not \
                  declared in MUTABLE_STATE_OWNERSHIP — declare its \
                  Repository|Worktree|Composite ownership (plan-20260714 §C.4.1.1, \
                  line 2246)"
            );
        }

        // REVERSE: a scoped declaration must be backed by the schema.
        for surface in MUTABLE_STATE_OWNERSHIP {
            match surface.owner {
                StateOwner::Worktree | StateOwner::Composite => assert!(
                    scoped_in_schema.contains(surface.table),
                    "`{}` is declared scope-carrying but no CREATE TABLE in the SQL \
                      corpus gives it a `worktree_id` column — a declaration that \
                      outran the schema",
                    surface.table
                ),
                StateOwner::Repository => {}
            }
            assert!(
                !surface.rationale.trim().is_empty(),
                "`{}` must record WHY it has this ownership",
                surface.table
            );
        }

        // The registry stays well-formed.
        let mut seen = BTreeSet::new();
        for surface in MUTABLE_STATE_OWNERSHIP {
            assert!(
                seen.insert(surface.table),
                "duplicate registry row for `{}`",
                surface.table
            );
        }

        // EXHAUSTIVE over persistent tables (§C.4.1.1 line 2246: "每个
        // mutable SQLite table … 必须在集中 inventory 中声明"). Scoped-only
        // coverage would let a new REPOSITORY-owned table ship
        // unclassified, since it carries no column to detect.
        let declared: BTreeSet<&str> = MUTABLE_STATE_OWNERSHIP
            .iter()
            .map(|surface| surface.table)
            .collect();
        let migration_only: BTreeSet<&str> = MIGRATION_ONLY_TABLES.iter().copied().collect();
        for table in all_created_tables() {
            assert!(
                declared.contains(table.as_str()) || migration_only.contains(table.as_str()),
                "mutable table `{table}` is not classified: add a row to \
                  MUTABLE_STATE_OWNERSHIP with its Repository|Worktree|Composite \
                  ownership, or — if a single migration creates AND drops it — to \
                  MIGRATION_ONLY_TABLES (plan-20260714 §C.4.1.1, line 2246)"
            );
        }
        // ...and no registry row may name a table the schema never creates
        // (`created` is the scan from the self-checks above).
        for surface in MUTABLE_STATE_OWNERSHIP {
            assert!(
                created.contains(surface.table),
                "registry row `{}` names a table no migration creates",
                surface.table
            );
        }
        for table in MIGRATION_ONLY_TABLES {
            assert!(
                created.contains(*table),
                "migration-only exclusion `{table}` names a table no migration creates"
            );
            assert!(
                !declared.contains(table),
                "`{table}` cannot be both classified and excluded"
            );
        }
    }
}
