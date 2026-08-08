//! Unit tests for the MCP resource and tool bridge.
//!
//! Scenario focus: command normalization, allowlisted Libra VCS execution, resource
//! URI parsing, and error envelopes that external MCP clients depend on.

use std::sync::Arc;

use sea_orm::{ConnectionTrait, Database, Schema};
use tempfile::tempdir;

// use uuid::Uuid;
use crate::{
    internal::{
        ai::{
            history::HistoryManager,
            mcp::{resource::CreateTaskParams, server::LibraMcpServer},
        },
        model::reference,
    },
    utils::storage::local::LocalStorage,
};

async fn setup_test_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let builder = db.get_database_backend();
    let schema = Schema::new(builder);
    let stmt = schema.create_table_from_entity(reference::Entity);
    db.execute_raw(builder.build(&stmt)).await.unwrap();
    // Present in every real repository (bootstrap schema) — the write-lock
    // primitive in `db::begin_write_transaction` writes against it.
    db.execute_raw(sea_orm::Statement::from_string(
        builder,
        "CREATE TABLE config_kv(id INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL,\
         value TEXT NOT NULL,encrypted INTEGER NOT NULL DEFAULT 0)"
            .to_string(),
    ))
    .await
    .unwrap();
    db
}

#[tokio::test]
async fn test_create_task_tool() {
    let temp_dir = tempdir().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path().join("objects")));
    let db_conn = Arc::new(setup_test_db().await);
    let history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        temp_dir.path().to_path_buf(),
        db_conn,
    ));
    let server = LibraMcpServer::new(Some(history_manager), Some(storage));

    let params = CreateTaskParams {
        title: "Test Task".to_string(),
        intent_id: None,
        description: Some("Description".to_string()),
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        parent_task_id: None,
        origin_step_id: None,
        status: None,
        reason: None,
        tags: None,
        external_ids: None,
        actor_kind: None,
        actor_id: None,
    };

    let actor = server.default_actor().unwrap();
    let result = server.create_task_impl(params, actor).await;
    assert!(result.is_ok());

    let call_result = result.unwrap();
    assert!(!call_result.content.is_empty());
    let content = &call_result.content[0];

    use rmcp::model::RawContent;
    match &content.raw {
        RawContent::Text(text_content) => {
            assert!(text_content.text.contains("Task created with ID"));
        }
        _ => panic!("Expected text content"),
    }
}

// ---------------------------------------------------------------------------
// Phase 5 authz wiring
// ---------------------------------------------------------------------------

use async_trait::async_trait;

use crate::internal::ai::{
    mcp::authz::{AuthzDecision, AuthzError, McpAuthorizer, McpOperation},
    runtime::hardening::PrincipalContext,
};

/// Test fixture: always returns `Deny { reason }` so a wired-in authz
/// gate flips a previously-OK impl call into an error.
struct DenyAllAuthz {
    reason: &'static str,
}

#[async_trait]
impl McpAuthorizer for DenyAllAuthz {
    async fn authorize(
        &self,
        _principal: &PrincipalContext,
        _operation: McpOperation<'_>,
    ) -> Result<AuthzDecision, AuthzError> {
        Ok(AuthzDecision::Deny {
            reason: self.reason.to_string(),
        })
    }
}

/// `list_resources_impl` without an authz handler installed must continue
/// to return its standard two-resource list (pre-Phase-5 behavior).
#[tokio::test]
async fn list_resources_impl_succeeds_without_authz() {
    let server = LibraMcpServer::new(None, None);

    let resources = server
        .list_resources_impl()
        .await
        .expect("list_resources_impl must succeed when no authz handler is installed");

    assert_eq!(resources.len(), 2);
}

/// With a `DenyAllAuthz` installed via [`LibraMcpServer::set_authz`], the
/// `list_resources_impl` call must surface the Deny reason as an
/// `invalid_request` error rather than returning the resource list.
#[tokio::test]
async fn list_resources_impl_is_blocked_by_deny_authz() {
    let server = LibraMcpServer::new(None, None);
    server.set_authz(Arc::new(DenyAllAuthz {
        reason: "test fixture denies everything",
    }));

    let err = server
        .list_resources_impl()
        .await
        .expect_err("list_resources_impl must propagate the deny decision");
    let message = err.message.to_string();
    assert!(
        message.contains("MCP authorization denied"),
        "error message should self-identify (got {message:?})"
    );
    assert!(
        message.contains("test fixture denies everything"),
        "deny reason should be preserved in the error message (got {message:?})"
    );
}

/// `read_resource_impl` with a `DenyAllAuthz` installed must surface the
/// deny reason before any history / context lookup happens. The fixture
/// uses a fake `uri` that wouldn't otherwise be recognized by the impl —
/// authz must intercept before the URI dispatch can return a "not found"
/// error.
#[tokio::test]
async fn read_resource_impl_is_blocked_by_deny_authz() {
    let server = LibraMcpServer::new(None, None);
    server.set_authz(Arc::new(DenyAllAuthz {
        reason: "deny reads",
    }));

    let err = server
        .read_resource_impl("libra://history/latest")
        .await
        .expect_err("read_resource_impl must surface the deny decision");
    let message = err.message.to_string();
    assert!(
        message.contains("MCP authorization denied"),
        "error message should self-identify (got {message:?})"
    );
    assert!(
        message.contains("deny reads"),
        "deny reason should be preserved (got {message:?})"
    );
}

/// `read_resource_impl` without an authz handler must continue to surface
/// its own internal errors (here: missing `intent_history_manager`)
/// instead of an authz error — proves the `Ok` fast-path doesn't mask
/// downstream failures.
#[tokio::test]
async fn read_resource_impl_without_authz_uses_existing_error_path() {
    let server = LibraMcpServer::new(None, None);

    let err = server
        .read_resource_impl("libra://history/latest")
        .await
        .expect_err("missing intent_history_manager must still error");
    let message = err.message.to_string();
    assert!(
        message.contains("History not available"),
        "without authz the existing 'History not available' path must run \
         (got {message:?})"
    );
}

/// `create_task_impl` with `DenyAllAuthz` installed must surface the
/// deny reason before any task creation logic runs. Validates that
/// `McpOperation::CallTool { tool_name: "create_task" }` flows through
/// the same gate as resource-side operations.
#[tokio::test]
async fn create_task_impl_is_blocked_by_deny_authz() {
    let temp_dir = tempdir().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path().join("objects")));
    let db_conn = Arc::new(setup_test_db().await);
    let history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        temp_dir.path().to_path_buf(),
        db_conn,
    ));
    let server = LibraMcpServer::new(Some(history_manager), Some(storage));
    server.set_authz(Arc::new(DenyAllAuthz {
        reason: "deny create_task",
    }));

    let params = CreateTaskParams {
        title: "Should not be created".to_string(),
        intent_id: None,
        description: Some("Authz denies this".to_string()),
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        parent_task_id: None,
        origin_step_id: None,
        status: None,
        reason: None,
        tags: None,
        external_ids: None,
        actor_kind: None,
        actor_id: None,
    };

    let actor = server.default_actor().unwrap();
    let err = server
        .create_task_impl(params, actor)
        .await
        .expect_err("create_task_impl must surface the deny decision");
    let message = err.message.to_string();
    assert!(
        message.contains("MCP authorization denied"),
        "error message should self-identify (got {message:?})"
    );
    assert!(
        message.contains("deny create_task"),
        "deny reason should be preserved (got {message:?})"
    );
}

/// Records every call's (op, principal) pair so a test can assert that
/// per-request principal threading actually delivers the caller's
/// `ActorRef` identity to the authz layer rather than collapsing to
/// `PrincipalContext::system()`.
struct RecordingAllowAuthz {
    last_principal: tokio::sync::Mutex<Option<PrincipalContext>>,
}

#[async_trait]
impl McpAuthorizer for RecordingAllowAuthz {
    async fn authorize(
        &self,
        principal: &PrincipalContext,
        _operation: McpOperation<'_>,
    ) -> Result<AuthzDecision, AuthzError> {
        *self.last_principal.lock().await = Some(principal.clone());
        Ok(AuthzDecision::Allow)
    }
}

/// `create_task_impl` must thread its `actor: ActorRef` parameter through
/// to the authz layer as a `PrincipalContext` derived via
/// [`PrincipalContext::from_actor`]. The `default_actor()` helper builds
/// an `ActorKind::McpClient` actor, which maps to
/// `PrincipalRole::Contributor` + `principal_id = "mcp-user"`.
#[tokio::test]
async fn create_task_impl_threads_actor_into_authz_principal() {
    use crate::internal::ai::runtime::hardening::PrincipalRole;

    let temp_dir = tempdir().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path().join("objects")));
    let db_conn = Arc::new(setup_test_db().await);
    let history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        temp_dir.path().to_path_buf(),
        db_conn,
    ));
    let server = LibraMcpServer::new(Some(history_manager), Some(storage));
    let recorder = Arc::new(RecordingAllowAuthz {
        last_principal: tokio::sync::Mutex::new(None),
    });
    server.set_authz(recorder.clone());

    let params = CreateTaskParams {
        title: "Test Task".to_string(),
        intent_id: None,
        description: Some("Description".to_string()),
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        parent_task_id: None,
        origin_step_id: None,
        status: None,
        reason: None,
        tags: None,
        external_ids: None,
        actor_kind: None,
        actor_id: None,
    };

    let actor = server.default_actor().unwrap();
    server
        .create_task_impl(params, actor)
        .await
        .expect("Allow authz must permit the create_task call");

    let recorded = recorder
        .last_principal
        .lock()
        .await
        .clone()
        .expect("authz must have been invoked with a principal");
    assert_eq!(
        recorded.principal_id, "mcp-user",
        "principal id should equal the ActorRef id from default_actor()"
    );
    assert_eq!(
        recorded.role,
        PrincipalRole::Contributor,
        "default_actor() builds an McpClient ActorRef which maps to Contributor"
    );
}
