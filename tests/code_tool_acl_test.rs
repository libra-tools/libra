//! Wave 8 / PR 8 — Tool registry ACL coverage for the
//! `libra code` `--context` modes (§5.9 first bullet).
//!
//! The Code UI runtime registers its first-party and semantic tools through
//! `CodeAgentServicesBuilder`. The
//! `--context` flag maps to a `TaskIntent` (Dev → Feature,
//! Review → Review, Research → Question), and
//! `ToolRegistry::filter_by_intent` defines the intent-filter policy used by
//! Code UI callers. These tests pin the production base registry and a real
//! source-prefixed dynamic registration and the legacy MCP bridge names against
//! that policy so a future
//! intent-mapping or registry change cannot silently classify a mutating tool
//! as review/research-safe. Runtime wiring remains covered by its own tool-loop
//! integration tests.
//!
//! `tool ACL × --approval-policy` tracking listed in §5.9 is
//! covered by `code_ui_remote_approval_matrix`; `--network-access
//! deny` is covered by the orchestrator policy and `web_search`
//! runtime tests because those gates live below the ACL filter.

use std::sync::Arc;

use libra::internal::ai::{
    agent::TaskIntent,
    mcp::server::LibraMcpServer,
    runtime::services::CodeAgentServicesBuilder,
    sources::{
        CapabilityManifest, Source, SourceCallContext, SourceKind, SourcePool,
        SourceToolCapability, SourceToolNaming, TrustTier,
    },
    tools::{ToolInvocation, ToolOutput, ToolSpec, handlers::McpBridgeHandler},
};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Clone)]
struct DynamicReadOnlySource {
    manifest: CapabilityManifest,
}

#[async_trait::async_trait]
impl Source for DynamicReadOnlySource {
    fn manifest(&self) -> &CapabilityManifest {
        &self.manifest
    }

    async fn call_tool(
        &self,
        _context: SourceCallContext,
        _invocation: ToolInvocation,
    ) -> libra::internal::ai::tools::ToolResult<ToolOutput> {
        Ok(ToolOutput::success("dynamic source lookup"))
    }
}

/// Build the actual production Web Code UI registry. Keeping test construction
/// on `CodeAgentServicesBuilder` means a registration change cannot leave the
/// ACL suite validating a stale hand-assembled tool set.
fn build_production_code_ui_registry() -> std::sync::Arc<libra::internal::ai::tools::ToolRegistry> {
    let dir = tempfile::tempdir().expect("tempdir for ACL test");
    let (user_input_tx, _user_input_rx) = mpsc::unbounded_channel();
    CodeAgentServicesBuilder::web_headless(dir.path(), Uuid::new_v4(), user_input_tx)
        .build()
        .registry()
}

/// `Dev` → `TaskIntent::Feature` lets every registered tool through
/// — including the mutating `apply_patch` / `shell` /
/// `submit_*_draft` set — so the agent can drive the full
/// implementation workflow. Pinning this contract guards against
/// a future intent-filter regression that would silently drop a
/// dev-mode tool.
#[test]
fn dev_context_filter_keeps_all_registered_tools() {
    let registry = build_production_code_ui_registry();
    let allowed = registry.filter_by_intent(TaskIntent::Feature);

    for required in [
        "read_file",
        "list_dir",
        "grep_files",
        "search_files",
        "web_search",
        "apply_patch",
        "shell",
        "update_plan",
        "submit_intent_draft",
        "submit_plan_draft",
    ] {
        assert!(
            allowed.iter().any(|name| name == required),
            "Dev/Feature filter dropped '{required}'; allowed: {allowed:?}",
        );
    }
}

/// `Review` → `TaskIntent::Review` MUST exclude any tool that can
/// mutate the workspace or shell out, because a review-context
/// session is supposed to inspect, not change. This test pins the
/// exclusion list so a regression that flipped `apply_patch` or
/// `shell` into the read-only allowlist would fail loud.
#[test]
fn review_context_filter_drops_mutating_tools() {
    let registry = build_production_code_ui_registry();
    let allowed = registry.filter_by_intent(TaskIntent::Review);

    for forbidden in [
        "apply_patch",
        "shell",
        "submit_intent_draft",
        "submit_plan_draft",
        "update_plan",
    ] {
        assert!(
            !allowed.iter().any(|name| name == forbidden),
            "Review filter must drop '{forbidden}', but allowed: {allowed:?}",
        );
    }

    for required in [
        "read_file",
        "list_dir",
        "grep_files",
        "search_files",
        "web_search",
    ] {
        assert!(
            allowed.iter().any(|name| name == required),
            "Review filter dropped read-only '{required}'; allowed: {allowed:?}",
        );
    }
}

/// `Research` → `TaskIntent::Question` shares the read-only-or-
/// semantic-tool predicate with Review, so the same exclusions
/// apply. Pin both contracts independently so Review and Research
/// can diverge in the future without a shared-test regression.
#[test]
fn research_context_filter_drops_mutating_tools() {
    let registry = build_production_code_ui_registry();
    let allowed = registry.filter_by_intent(TaskIntent::Question);

    for forbidden in ["apply_patch", "shell"] {
        assert!(
            !allowed.iter().any(|name| name == forbidden),
            "Research filter must drop '{forbidden}', but allowed: {allowed:?}",
        );
    }
    assert!(allowed.iter().any(|name| name == "read_file"));
    assert!(allowed.iter().any(|name| name == "list_dir"));
    assert!(allowed.iter().any(|name| name == "grep_files"));
    assert!(allowed.iter().any(|name| name == "web_search"));
}

/// Default (`None` context) → `TaskIntent::Unknown` keeps every
/// tool — the runtime relies on a downstream auto-classifier to
/// pick the actual intent on the first user message, and pre-
/// filtering would defeat that path.
#[test]
fn unknown_intent_keeps_all_registered_tools() {
    let registry = build_production_code_ui_registry();
    let allowed = registry.filter_by_intent(TaskIntent::Unknown);

    for required in ["apply_patch", "shell", "read_file", "submit_intent_draft"] {
        assert!(
            allowed.iter().any(|name| name == required),
            "Unknown filter dropped '{required}'; allowed: {allowed:?}",
        );
    }
}

/// `Command` → `TaskIntent::Command` lets `shell` through but no
/// other mutating tool — this is the special "run a single shell
/// command" intent surfaced by the auto-classifier and the
/// allowlist contract is documented in
/// `tool_allowed_for_intent` in `tools/registry.rs`.
#[test]
fn command_intent_allows_shell_only_among_mutating_tools() {
    let registry = build_production_code_ui_registry();
    let allowed = registry.filter_by_intent(TaskIntent::Command);

    assert!(
        allowed.iter().any(|name| name == "shell"),
        "Command filter must keep 'shell'; allowed: {allowed:?}"
    );
    for forbidden in [
        "apply_patch",
        "submit_intent_draft",
        "submit_plan_draft",
        "update_plan",
    ] {
        assert!(
            !allowed.iter().any(|name| name == forbidden),
            "Command filter must drop '{forbidden}', but allowed: {allowed:?}",
        );
    }
}

/// Source tools are injected into an effective run registry after the base
/// Code UI services have been built. A prefixed source tool has no semantic
/// allowlist entry, so Review/Research must default-deny it instead of treating
/// an arbitrary dynamically supplied name as read-only.
#[test]
fn review_and_research_filters_default_deny_prefixed_dynamic_source_tools() {
    let mut registry = (*build_production_code_ui_registry()).clone();
    let source = DynamicReadOnlySource {
        manifest: CapabilityManifest::new(
            "project_docs",
            SourceKind::LocalDocs,
            TrustTier::Project,
        )
        .with_tool(SourceToolCapability::new(
            "lookup",
            ToolSpec::new("lookup", "read a project document"),
        )),
    };
    let sources = SourcePool::new();
    sources
        .register_source(std::sync::Arc::new(source))
        .expect("project source registration must succeed");
    for (name, handler) in sources
        .tool_handlers_for_session("acl-test", SourceToolNaming::Prefixed)
        .expect("source handlers must be materialized with production naming")
    {
        registry.register(name, handler);
    }

    for intent in [TaskIntent::Review, TaskIntent::Question] {
        let allowed = registry.filter_by_intent(intent);
        assert!(
            !allowed.iter().any(|name| name == "project_docs__lookup"),
            "{intent:?} must default-deny dynamic source tools; allowed: {allowed:?}",
        );
    }
}

/// The SourcePool path above protects production's dynamically prefixed tools.
/// Keep this legacy bridge-name assertion as well: it verifies the intent ACL
/// remains conservative for the real mutating MCP names while the aggregate
/// bridge is retained as a migration test seam.
#[test]
fn review_and_research_filters_drop_real_mcp_bridge_mutating_tools() {
    let mut registry = (*build_production_code_ui_registry()).clone();
    for (name, handler) in McpBridgeHandler::all_handlers(Arc::new(LibraMcpServer::new(None, None)))
    {
        registry.register(name, handler);
    }

    let mutating_mcp_names = registry
        .filter_by_intent(TaskIntent::Feature)
        .into_iter()
        .filter(|name| {
            name == "run_libra_vcs" || name.starts_with("create_") || name.starts_with("update_")
        })
        .collect::<Vec<_>>();
    assert!(
        !mutating_mcp_names.is_empty(),
        "the real MCP bridge must expose mutating tool names"
    );

    for intent in [TaskIntent::Review, TaskIntent::Question] {
        let allowed = registry.filter_by_intent(intent);
        for name in &mutating_mcp_names {
            assert!(
                !allowed.iter().any(|allowed_name| allowed_name == name),
                "{intent:?} must drop mutating MCP tool '{name}'; allowed: {allowed:?}",
            );
        }
    }
}
