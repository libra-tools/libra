//! KEEP permission shape for inheritance — plan-20260920 RC-19.
//!
//! `permission/inheritance.rs` depends on this module instead of
//! `agent::profile::AgentPermissionSpec`. The agent profile re-exports
//! the same types until RC-23.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Where approval prompts route when the agent invokes a tool that requires
/// human consent. Mirrors
/// [`crate::internal::ai::agent_run::permission::ApprovalRouting`] so the
/// feature-gated runtime conversion stays purely structural; do not reorder
/// variants without keeping the gated module in lock-step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRoutingSpec {
    /// All approvals route to Layer 1 / human reviewer.
    #[default]
    Layer1Human,
    /// Pre-approved for the duration of this run. Used by read-only sub-agents
    /// like `explore` where an interactive prompt would be pure friction.
    SessionPreApproved,
}

/// Permission shape attached to an `AgentExecutionSpec`.
///
/// Field names, container types (`BTreeSet<String>`) and defaults mirror the
/// feature-gated
/// [`crate::internal::ai::agent_run::permission::AgentPermissionProfile`] so
/// the OC-Phase 3 runtime can convert one into the other without renaming or
/// re-deduplicating. This struct is available in the **default** build (no
/// `subagent-scaffold` feature required) so config and parsing code can use it
/// before the dispatcher lands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPermissionSpec {
    /// Tool names this agent may invoke. Empty = deny everything.
    #[serde(default)]
    pub allowed_tools: BTreeSet<String>,

    /// Hard denies that override `allowed_tools` even on partial overlap.
    #[serde(default)]
    pub denied_tools: BTreeSet<String>,

    /// Source Pool slugs the agent may read from.
    #[serde(default)]
    pub allowed_source_slugs: BTreeSet<String>,

    /// Where approval prompts route. Defaults to Layer1Human per S2-INV-06.
    #[serde(default)]
    pub approval_routing: ApprovalRoutingSpec,

    /// Whether this agent may spawn further sub-agents through `task`.
    /// Per S2-INV-09 this is `false` by default; only Layer 1 is a legitimate
    /// spawner unless an operator explicitly opts in via config.
    #[serde(default)]
    pub may_spawn_sub_agents: bool,
}

impl AgentPermissionSpec {
    /// Whether `tool` may be invoked under this permission spec.
    ///
    /// Encodes the same S2-INV-05 contract as the feature-gated
    /// [`AgentPermissionProfile::permits_tool`] so the default-build
    /// config / parsing layer evaluates tool gating identically to the
    /// runtime profile it converts into:
    ///
    /// - **default deny** — an empty spec permits nothing; a tool must
    ///   be explicitly present in `allowed_tools`;
    /// - **deny wins** — a tool in `denied_tools` is rejected even if it
    ///   also appears in `allowed_tools`.
    ///
    /// Tool names are matched **exactly** against the `BTreeSet`s (no
    /// `*` wildcard / glob / prefix matching). A tool is permitted iff
    /// it is in `allowed_tools` AND not in `denied_tools`.
    ///
    /// [`AgentPermissionProfile::permits_tool`]: crate::internal::ai::agent_run::permission::AgentPermissionProfile::permits_tool
    pub fn permits_tool(&self, tool: &str) -> bool {
        !self.denied_tools.contains(tool) && self.allowed_tools.contains(tool)
    }
}
