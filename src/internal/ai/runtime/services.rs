//! UI-neutral construction of the services used by a Code agent turn.
//!
//! CLI adapters choose a launch profile and supply their channels, but they do
//! not assemble a second tool registry or hardening boundary.  Provider/model
//! construction remains in the command layer because it owns clap arguments
//! and the Vault/env lookup; both TUI and headless paths feed that same
//! provider factory.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::sync::mpsc;
use uuid::Uuid;

use super::{ToolBoundaryRuntime, TracingAuditSink};
use crate::internal::ai::{
    mcp::server::LibraMcpServer,
    sandbox::{
        ApprovalCachePolicy, ApprovalStore, AskForApproval, ExecApprovalRequest, NetworkAccess,
        SandboxPermissions, SandboxPolicy, ToolApprovalContext, ToolRuntimeContext,
        ToolSandboxContext,
    },
    tools::{
        ToolRegistry, ToolRegistryBuilder,
        context::UserInputRequest,
        handlers::{
            ApplyPatchHandler, GrepFilesHandler, ListDirHandler, McpBridgeHandler, PlanHandler,
            ReadFileHandler, RequestUserInputHandler, SearchFilesHandler, ShellHandler,
            SubmitIntentDraftHandler, SubmitPlanDraftHandler, SubmitTaskCompleteHandler,
            WebSearchHandler, register_semantic_handlers,
        },
    },
};

/// The capability set an adapter may expose at launch.  These names describe
/// behavior, not a dependency on a terminal implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeAgentLaunchProfile {
    /// Full interactive workflow, including Intent/Task tools and MCP bridge
    /// tools that use the shared in-process MCP server.
    TuiBaseline,
    /// Direct-turn browser workflow.  Workflow tools requiring a session
    /// driver remain unavailable until their dedicated migration tasks.
    WebHeadless,
}

/// Shared, bounded Code-agent services.  Adapters can consume the registry
/// but cannot replace its hardening configuration after launch.
#[derive(Clone)]
pub struct CodeAgentServices {
    profile: CodeAgentLaunchProfile,
    registry: Arc<ToolRegistry>,
}

impl CodeAgentServices {
    pub fn profile(&self) -> CodeAgentLaunchProfile {
        self.profile
    }

    pub fn registry(&self) -> Arc<ToolRegistry> {
        self.registry.clone()
    }
}

/// Builds the one Code-agent registry for a launch profile.
pub struct CodeAgentServicesBuilder {
    launch: CodeAgentServicesLaunch,
    working_dir: PathBuf,
    trace_id: Uuid,
    user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
}

enum CodeAgentServicesLaunch {
    TuiBaseline { mcp_server: Arc<LibraMcpServer> },
    WebHeadless,
}

impl CodeAgentServicesBuilder {
    pub fn tui_baseline(
        working_dir: impl Into<PathBuf>,
        trace_id: Uuid,
        user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
        mcp_server: Arc<LibraMcpServer>,
    ) -> Self {
        Self {
            launch: CodeAgentServicesLaunch::TuiBaseline { mcp_server },
            working_dir: working_dir.into(),
            trace_id,
            user_input_tx,
        }
    }

    pub fn web_headless(
        working_dir: impl Into<PathBuf>,
        trace_id: Uuid,
        user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
    ) -> Self {
        Self {
            launch: CodeAgentServicesLaunch::WebHeadless,
            working_dir: working_dir.into(),
            trace_id,
            user_input_tx,
        }
    }

    /// Construct the registry with the shared hardening/audit boundary.
    pub fn build(self) -> CodeAgentServices {
        let Self {
            launch,
            working_dir,
            trace_id,
            user_input_tx,
        } = self;
        let profile = match &launch {
            CodeAgentServicesLaunch::TuiBaseline { .. } => CodeAgentLaunchProfile::TuiBaseline,
            CodeAgentServicesLaunch::WebHeadless => CodeAgentLaunchProfile::WebHeadless,
        };
        let mut builder = ToolRegistryBuilder::with_working_dir(working_dir)
            .hardening(ToolBoundaryRuntime::system(
                trace_id,
                Arc::new(TracingAuditSink),
            ))
            .register("read_file", Arc::new(ReadFileHandler))
            .register("list_dir", Arc::new(ListDirHandler))
            .register("grep_files", Arc::new(GrepFilesHandler))
            .register("search_files", Arc::new(SearchFilesHandler))
            .register("web_search", Arc::new(WebSearchHandler))
            .register("apply_patch", Arc::new(ApplyPatchHandler))
            .register("shell", Arc::new(ShellHandler))
            .register("update_plan", Arc::new(PlanHandler))
            .register(
                "request_user_input",
                Arc::new(RequestUserInputHandler::new(user_input_tx)),
            );

        match launch {
            CodeAgentServicesLaunch::TuiBaseline { mcp_server } => {
                builder = builder
                    .register("submit_intent_draft", Arc::new(SubmitIntentDraftHandler))
                    .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
                    .register("submit_task_complete", Arc::new(SubmitTaskCompleteHandler));
                for (name, handler) in McpBridgeHandler::all_handlers(mcp_server) {
                    builder = builder.register(name, handler);
                }
            }
            CodeAgentServicesLaunch::WebHeadless => {
                builder = builder.register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler));
            }
        }

        CodeAgentServices {
            profile,
            registry: Arc::new(register_semantic_handlers(builder).build()),
        }
    }
}

/// UI-neutral sandbox policy selected by the command adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeAgentSandboxProfile {
    WorkspaceWrite { network_access: bool },
    ReadOnly,
}

/// Approval settings resolved from CLI and project configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeAgentApprovalConfig {
    pub policy: AskForApproval,
    pub allow_all_commands: bool,
    pub ttl: Duration,
    pub cache_policy: ApprovalCachePolicy,
}

/// Construct the sole sandbox/approval context used by both interactive and
/// headless Code launches.
pub fn tool_runtime_context(
    working_dir: &Path,
    sandbox_profile: CodeAgentSandboxProfile,
    approval: CodeAgentApprovalConfig,
    exec_approval_tx: mpsc::UnboundedSender<ExecApprovalRequest>,
) -> ToolRuntimeContext {
    let policy = match sandbox_profile {
        CodeAgentSandboxProfile::WorkspaceWrite { network_access } => {
            SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![working_dir.to_path_buf()],
                network_access: NetworkAccess::from_legacy_bool(network_access),
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: false,
                allow_metadata_writes: false,
            }
        }
        CodeAgentSandboxProfile::ReadOnly => SandboxPolicy::ReadOnly,
    };

    let mut approval_store = ApprovalStore::default();
    if approval.allow_all_commands {
        approval_store.approve_all_commands();
    }

    ToolRuntimeContext {
        sandbox: Some(ToolSandboxContext {
            policy,
            permissions: SandboxPermissions::UseDefault,
        }),
        sandbox_runtime: None,
        approval: Some(ToolApprovalContext {
            policy: approval.policy,
            request_tx: exec_approval_tx,
            store: Arc::new(tokio::sync::Mutex::new(approval_store)),
            scope_key_prefix: None,
            approval_ttl: approval.ttl,
            cache_policy: approval.cache_policy,
        }),
        file_history: None,
        max_output_bytes: None,
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    #[test]
    fn headless_profile_keeps_the_shared_hardening_boundary() {
        let temp_dir = tempfile::tempdir().expect("temporary working directory");
        let (user_input_tx, _user_input_rx) = mpsc::unbounded_channel();
        let services =
            CodeAgentServicesBuilder::web_headless(temp_dir.path(), Uuid::new_v4(), user_input_tx)
                .build();

        let registry = services.registry();
        assert_eq!(services.profile(), CodeAgentLaunchProfile::WebHeadless);
        assert!(registry.hardening().is_some());
        assert!(registry.handler("apply_patch").is_some());
        assert!(registry.handler("submit_plan_draft").is_some());
        assert!(registry.handler("submit_intent_draft").is_none());
    }

    #[tokio::test]
    async fn runtime_context_keeps_sandbox_and_approval_together() {
        let temp_dir = tempfile::tempdir().expect("temporary working directory");
        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let context = tool_runtime_context(
            temp_dir.path(),
            CodeAgentSandboxProfile::WorkspaceWrite {
                network_access: true,
            },
            CodeAgentApprovalConfig {
                policy: AskForApproval::UnlessTrusted,
                allow_all_commands: true,
                ttl: Duration::from_secs(30),
                cache_policy: ApprovalCachePolicy::default(),
            },
            approval_tx,
        );

        assert!(matches!(
            context.sandbox.expect("sandbox context").policy,
            SandboxPolicy::WorkspaceWrite {
                network_access: NetworkAccess::Full,
                ..
            }
        ));
        let approval = context.approval.expect("approval context");
        assert_eq!(approval.policy, AskForApproval::UnlessTrusted);
        assert!(approval.store.lock().await.allow_all_commands());
    }
}
