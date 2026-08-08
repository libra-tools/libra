//! OC-Phase "EntireIO 兼容" compatibility gate (opencode.md:1608 test matrix
//! row, and the `兼容性 Gate` bullet at opencode.md:156-157).
//!
//! The CEX-S2 sub-agent runtime (Step 2) and the CEX-EntireIO observed-agent
//! capture runtime (`agent_session` / `agent_checkpoint` / `refs/libra/
//! traces`) are two *separate* subsystems. The documented gate requires
//! that running a sub-agent must NOT write the EntireIO observed-agent stores:
//!
//! > fake sub-agent E2E 只产生 `AgentRunEvent` / code session JSONL / usage row,
//! > 不产生 external agent checkpoint.
//!
//! The only writer of `agent_session` / `agent_checkpoint` is the observed-agent
//! hook ingest path (`process_hook_event_with_target(HookTarget::AgentTraces)`
//! → `ingest_agent_traces` in `src/internal/ai/hooks/runtime.rs`), which the
//! sub-agent dispatcher never calls. This test pins that decoupling so a future
//! change that coupled the dispatcher to the EntireIO writers — leaking
//! observed-agent rows for an internal sub-agent run — fails here.
//!
//! The existing `ai_subagent_flag_off_regression_test` pins the *config-default*
//! half of the gate; this file pins the *runtime no-cross-write* half.

#![cfg(feature = "test-provider")]

use std::{path::PathBuf, sync::Arc};

use libra::internal::{
    ai::{
        agent::{
            profile::{
                AgentExecutionSpec, AgentMode, AgentPermissionSpec, ModelBinding, ToolSelection,
            },
            runtime::{
                AbortToken, AgentSpecRegistry, ContextFrameLoader, DefaultSubAgentDispatcher,
                DispatchContext, MessageId, MultiAgentConfig, PermissionAskRequest,
                PermissionAsker, PermissionReply, PermissionService, SubAgentDispatcher,
                TaskEntryKind, TaskInvocation,
            },
        },
        agent_run::AgentRunEvent,
        providers::{ProviderBuildOptions, ProviderFactory},
        session::{
            SessionId,
            jsonl::{SessionEvent, SessionJsonlStore},
        },
        tools::ToolRegistry,
        usage::UsageRecorder,
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::{ConnectionTrait, Statement};

fn fixture_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/sub_agent/explore_simple.json");
    path
}

fn explore_sub_agent_spec() -> AgentExecutionSpec {
    AgentExecutionSpec {
        name: "explore".to_string(),
        description: "Read-only explorer (EntireIO-compat fixture)".to_string(),
        mode: AgentMode::Subagent,
        model: ModelBinding::parse("fake/some-model"),
        tools: ToolSelection::Inherit,
        permission: AgentPermissionSpec::default(),
        ..AgentExecutionSpec::default()
    }
}

fn parent_spec() -> AgentExecutionSpec {
    AgentExecutionSpec {
        name: "parent".to_string(),
        description: "EntireIO-compat fixture parent".to_string(),
        mode: AgentMode::Primary,
        model: ModelBinding::parse("fake/parent-model"),
        ..AgentExecutionSpec::default()
    }
}

/// A never-asked asker: UserInitiated{bypass} skips it. Present only to satisfy
/// the `PermissionService` constructor.
struct UnusedAsker;

impl PermissionAsker for UnusedAsker {
    fn ask<'a>(
        &'a self,
        _request: PermissionAskRequest<'a>,
    ) -> futures::future::BoxFuture<'a, PermissionReply> {
        Box::pin(async move {
            PermissionReply::Reject {
                feedback: Some("asker must not be reached in this gate".to_string()),
            }
        })
    }
}

struct StaticRegistry {
    spec: AgentExecutionSpec,
}

impl AgentSpecRegistry for StaticRegistry {
    fn lookup(&self, name: &str) -> Option<AgentExecutionSpec> {
        if name == self.spec.name {
            Some(self.spec.clone())
        } else {
            None
        }
    }

    fn registered_names(&self) -> Vec<String> {
        vec![self.spec.name.clone()]
    }
}

async fn count_rows(conn: &sea_orm::DatabaseConnection, table: &str) -> i64 {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_string(
            backend,
            format!("SELECT COUNT(*) AS n FROM {table}"),
        ))
        .await
        .unwrap_or_else(|err| panic!("count {table}: {err}"))
        .unwrap_or_else(|| panic!("count {table}: no row"));
    row.try_get_by::<i64, _>("n")
        .unwrap_or_else(|err| panic!("read count {table}: {err}"))
}

/// A fake sub-agent dispatch produces `AgentRunEvent` JSONL but writes ZERO rows
/// to the EntireIO observed-agent tables (`agent_session` / `agent_checkpoint`).
#[tokio::test]
async fn fake_sub_agent_run_does_not_write_entire_io_observed_agent_tables() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());
    let conn = sea_orm::Database::connect("sqlite::memory:")
        .await
        .expect("sqlite memory db");
    // Migrate so the EntireIO tables exist — the invariant is "they stay empty",
    // not "they are absent".
    run_builtin_migrations(&conn).await.expect("migrations");

    // Baseline: the observed-agent tables start empty.
    assert_eq!(count_rows(&conn, "agent_session").await, 0);
    assert_eq!(count_rows(&conn, "agent_checkpoint").await, 0);

    let usage_recorder = UsageRecorder::new(conn.clone());
    let context_frame_loader = ContextFrameLoader::default();
    let permission_service =
        PermissionService::new(Arc::new(UnusedAsker) as Arc<dyn PermissionAsker>);
    let provider_factory = ProviderFactory;
    let provider_options = ProviderBuildOptions {
        fake_fixture_path: Some(fixture_path()),
        ..ProviderBuildOptions::default()
    };
    let tool_registry = ToolRegistry::with_working_dir(temp.path().to_path_buf());

    let parent = parent_spec();
    let parent_ruleset = Vec::new();
    let parent_binding = parent.model.clone().expect("parent binding");
    let session_id: SessionId = "session-entire-compat".to_string();

    let registry = Arc::new(StaticRegistry {
        spec: explore_sub_agent_spec(),
    });
    let dispatcher = DefaultSubAgentDispatcher::new(
        registry,
        MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        },
    )
    .with_default_child_runner();

    let abort_token = AbortToken::new();
    let context = DispatchContext {
        parent_thread_id: "thread-entire-compat",
        parent_session_id: &session_id,
        parent_agent: &parent,
        parent_ruleset: &parent_ruleset,
        parent_model_binding: &parent_binding,
        parent_message_id: MessageId::from("msg-entire-compat"),
        permission_service: &permission_service,
        session_store: &store,
        provider_factory: &provider_factory,
        provider_build_options: &provider_options,
        provider_build_options_resolver: None,
        tool_registry: &tool_registry,
        runtime_context: None,
        usage_recorder: &usage_recorder,
        context_frame_loader: &context_frame_loader,
        abort_token,
        depth: 0,
        compaction_model: None,
        hook_runner: None,
    };

    let invocation = TaskInvocation {
        description: "entire-io compat dispatch".to_string(),
        prompt: "summarise the repo".to_string(),
        subagent_type: "explore".to_string(),
        task_id: None,
    };

    let result = dispatcher
        .dispatch(
            context,
            invocation,
            TaskEntryKind::UserInitiated {
                bypass_permission_ask: true,
            },
        )
        .await
        .expect("fake sub-agent dispatch must succeed");
    assert_eq!(result.agent_name, "explore");

    // The sub-agent run DID produce its own `AgentRunEvent` JSONL (the runtime
    // ran, so the no-cross-write assertion below is meaningful, not vacuous).
    let agent_run_events: Vec<AgentRunEvent> = store
        .load_events()
        .expect("session JSONL must be readable")
        .into_iter()
        .filter_map(|envelope| match envelope {
            SessionEvent::AgentRun(known) => known.known().cloned(),
            _ => None,
        })
        .collect();
    assert!(
        agent_run_events
            .iter()
            .any(|event| matches!(event, AgentRunEvent::Spawned { .. }))
            && agent_run_events
                .iter()
                .any(|event| matches!(event, AgentRunEvent::Completed { .. })),
        "fake sub-agent run must emit its own Spawned + Completed AgentRunEvent JSONL; got {agent_run_events:?}",
    );

    // The gate: the run wrote NOTHING to the EntireIO observed-agent tables.
    assert_eq!(
        count_rows(&conn, "agent_session").await,
        0,
        "sub-agent dispatch must not write any `agent_session` rows (EntireIO cross-write leak)",
    );
    assert_eq!(
        count_rows(&conn, "agent_checkpoint").await,
        0,
        "sub-agent dispatch must not write any `agent_checkpoint` rows (EntireIO cross-write leak)",
    );
}
