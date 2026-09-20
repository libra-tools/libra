//! `AgentPermissionProfile[S]` snapshot.
//!
//! Per S2-INV-05 sub-agent tool policy is **default deny**; every Worker /
//! Explorer / Reviewer agent must declare its allowed tools, sources, and
//! approval mode explicitly. The exact `ApprovalKey` shape is owned by Step
//! 1.6 (Approval TTL) — until that lands, this profile uses a forward-stable
//! placeholder.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Approval policy scope for a sub-agent.
///
/// The full `ApprovalKey` (with `sensitivity_tier` / `scope` / `blast_radius`
/// fields) is frozen by Step 1.6. The variants here only express *who* the
/// approval prompts go to; field-level scope plumbing arrives later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalRouting {
    /// All sub-agent approvals route to Layer 1 / human reviewer.
    /// Default for all agent types per S2-INV-06.
    Layer1Human,
    /// Pre-approved for the duration of this `AgentRun` (used for read-only
    /// agents like Explorer).
    SessionPreApproved,
}

/// Permission profile attached to an `AgentRun`. Default deny: an empty
/// profile authorizes nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPermissionProfile {
    /// Tool names the sub-agent may call (subset of registered tools).
    /// Default empty = deny everything.
    #[serde(default)]
    pub allowed_tools: BTreeSet<String>,

    /// Tool names the sub-agent must NOT call even if `allowed_tools` would
    /// otherwise cover them (used for hard-coded denies like
    /// `spawn_subagent`). Matched by exact name — see [`permits_tool`];
    /// there is no `*` wildcard.
    ///
    /// [`permits_tool`]: AgentPermissionProfile::permits_tool
    #[serde(default)]
    pub denied_tools: BTreeSet<String>,

    /// Source Pool slugs the sub-agent may read from. Per CEX-S2-10,
    /// the slug namespace itself is owned by Step 1.10.
    #[serde(default)]
    pub allowed_source_slugs: BTreeSet<String>,

    /// Where approval prompts route.
    pub approval_routing: ApprovalRouting,

    /// Whether this sub-agent may spawn further sub-agents. Per S2-INV-09
    /// this is `false` by default; Layer 1 is the only legitimate spawner.
    #[serde(default)]
    pub may_spawn_sub_agents: bool,
}

impl Default for AgentPermissionProfile {
    fn default() -> Self {
        Self {
            allowed_tools: BTreeSet::new(),
            denied_tools: BTreeSet::new(),
            allowed_source_slugs: BTreeSet::new(),
            approval_routing: ApprovalRouting::Layer1Human,
            may_spawn_sub_agents: false,
        }
    }
}

impl AgentPermissionProfile {
    /// Whether `tool` may be invoked by a sub-agent running under this
    /// profile.
    ///
    /// Encodes the S2-INV-05 tool-gating contract directly so callers
    /// never re-derive it (and can't accidentally invert the
    /// precedence):
    ///
    /// - **default deny** — an empty profile (e.g. [`Default`]) permits
    ///   nothing; a tool must be explicitly listed in `allowed_tools`.
    /// - **deny wins** — a tool in `denied_tools` is rejected even if it
    ///   also appears in `allowed_tools`. This is what makes hard-coded
    ///   denies (e.g. `spawn_subagent`) unbypassable by a permissive
    ///   allow list, and is the local enforcement point for the
    ///   "parent deny always wins" inheritance rule.
    ///
    /// Tool names are matched **exactly** against the `BTreeSet`s —
    /// there is no glob / prefix matching, so a deny of `spawn_subagent`
    /// does not cover a differently-named `spawn_worker`. Callers that
    /// need a family-wide deny must list each member explicitly.
    ///
    /// A tool is permitted iff it is in `allowed_tools` AND not in
    /// `denied_tools`.
    pub fn permits_tool(&self, tool: &str) -> bool {
        !self.denied_tools.contains(tool) && self.allowed_tools.contains(tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(allowed: &[&str], denied: &[&str]) -> AgentPermissionProfile {
        AgentPermissionProfile {
            allowed_tools: allowed.iter().map(|t| t.to_string()).collect(),
            denied_tools: denied.iter().map(|t| t.to_string()).collect(),
            ..AgentPermissionProfile::default()
        }
    }

    /// S2-INV-05 default deny: the `Default` (empty) profile must
    /// permit no tool at all. Pin so a future `Default` that
    /// pre-populates `allowed_tools` can't silently widen sub-agent
    /// authority.
    #[test]
    fn default_profile_permits_nothing() {
        let profile = AgentPermissionProfile::default();
        for tool in [
            "read_file",
            "shell",
            "apply_patch",
            "web_search",
            "spawn_subagent",
        ] {
            assert!(
                !profile.permits_tool(tool),
                "default-deny profile must reject `{tool}`",
            );
        }
    }

    /// A tool explicitly allowed (and not denied) is permitted; a tool
    /// absent from both lists is denied (default deny).
    #[test]
    fn permits_only_explicitly_allowed_tools() {
        let profile = profile(&["read_file", "grep"], &[]);
        assert!(profile.permits_tool("read_file"));
        assert!(profile.permits_tool("grep"));
        // Not listed anywhere → default deny.
        assert!(!profile.permits_tool("shell"));
        assert!(!profile.permits_tool("apply_patch"));
    }

    /// **Deny wins**: a tool present in BOTH `allowed_tools` and
    /// `denied_tools` is rejected. This is the unbypassable hard-deny
    /// property (e.g. `spawn_subagent`) and the local enforcement of
    /// the "parent deny always wins" inheritance rule — pin it so a
    /// refactor that checks `allowed` first and short-circuits can't
    /// invert the precedence.
    #[test]
    fn denied_tool_overrides_allowed() {
        let profile = profile(&["read_file", "shell"], &["shell"]);
        assert!(
            profile.permits_tool("read_file"),
            "allowed-and-not-denied tool stays permitted",
        );
        assert!(
            !profile.permits_tool("shell"),
            "deny must win when a tool is in both allow and deny lists",
        );
    }

    /// A tool listed only in `denied_tools` (never allowed) is denied —
    /// the deny list never accidentally grants access.
    #[test]
    fn denied_only_tool_is_rejected() {
        let profile = profile(&[], &["spawn_subagent"]);
        assert!(!profile.permits_tool("spawn_subagent"));
        // And default deny still covers everything else.
        assert!(!profile.permits_tool("read_file"));
    }
}
