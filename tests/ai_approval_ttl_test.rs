//! CEX-11 approval TTL and canonical key contract tests.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chrono::{TimeZone, Utc};
use libra::{
    internal::{
        ai::{
            permission::{
                ApprovalProvenance, ApprovedRulesetStore, approval_cache_scope_key,
                resolve_approval_runtime_cache,
            },
            runtime::{
                AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExecutionControlService,
                ExternalTurnTrackingExecutor, InMemoryAuditSink, InteractionResponse,
                InteractionState, PrincipalContext, PrincipalRole, SecretRedactor,
                ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
            },
            sandbox::{
                ApprovalCachePolicy, ApprovalScope, ApprovalSensitivityTier, ApprovalStore,
                AskForApproval, ExecApprovalRequest, NetworkAccess, ReviewDecision,
                SandboxPermissions, ToolApprovalContext, request_cached_approval_with_keys,
                shell_approval_key, shell_approval_key_with_scope,
            },
            web::{
                AgentRuntimeCodeUiAdapter,
                code_ui::{
                    CodeUiCapabilities, CodeUiInitialController, CodeUiInteractionKind,
                    CodeUiInteractionRequest, CodeUiInteractionStatus, CodeUiProviderInfo,
                    CodeUiRuntimeHandle, CodeUiRuntimeOptions, CodeUiSession, CodeUiSessionStatus,
                    initial_snapshot,
                },
            },
        },
        db::get_db_conn_instance_for_path,
        worktree_scope::RequestScope,
    },
    utils::{test::setup_with_new_libra_in, util},
};
use tokio::sync::{Mutex, mpsc::error::TryRecvError};
use uuid::Uuid;

#[test]
fn canonical_shell_key_is_stable_for_flag_order_and_scope_fields() {
    let first = shell_approval_key(
        "cargo test --features test-provider --all-targets",
        Path::new("/workspace"),
        SandboxPermissions::UseDefault,
    );
    let second = shell_approval_key(
        "cargo test --all-targets --features test-provider",
        Path::new("/workspace"),
        SandboxPermissions::UseDefault,
    );
    let other_cwd = shell_approval_key(
        "cargo test --all-targets --features test-provider",
        Path::new("/other"),
        SandboxPermissions::UseDefault,
    );
    let escalated = shell_approval_key(
        "cargo test --all-targets --features test-provider",
        Path::new("/workspace"),
        SandboxPermissions::RequireEscalated,
    );

    assert_eq!(first, second);
    assert_ne!(first, other_cwd);
    assert_ne!(first, escalated);
    assert_eq!(first.len(), 64);
    assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn approval_store_expires_ttl_memos_and_keeps_session_memos() {
    let now = Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap();
    let mut store = ApprovalStore::default();

    store.put_ttl(
        "ttl-key".to_string(),
        ReviewDecision::ApprovedForTtl,
        ApprovalScope::Session,
        ApprovalSensitivityTier::Strict,
        now,
        Duration::from_secs(60),
    );
    store.put(
        "session-key".to_string(),
        ReviewDecision::ApprovedForSession,
    );

    assert_eq!(
        store.get_at("ttl-key", now + chrono::Duration::seconds(59)),
        Some(ReviewDecision::ApprovedForTtl)
    );
    assert_eq!(
        store.get_at("ttl-key", now + chrono::Duration::seconds(61)),
        None
    );
    assert_eq!(
        store.get_at("session-key", now + chrono::Duration::days(30)),
        Some(ReviewDecision::ApprovedForSession)
    );
}

#[test]
fn approval_key_changes_when_scope_or_sensitivity_tier_changes() {
    let strict_session = shell_approval_key_with_scope(
        "cargo test --all-targets",
        Path::new("/workspace"),
        SandboxPermissions::UseDefault,
        ApprovalScope::Session,
        ApprovalSensitivityTier::Strict,
    );
    let directory_session = shell_approval_key_with_scope(
        "cargo test --all-targets",
        Path::new("/workspace"),
        SandboxPermissions::UseDefault,
        ApprovalScope::Session,
        ApprovalSensitivityTier::Directory,
    );
    let pattern_project = shell_approval_key_with_scope(
        "cargo test --all-targets",
        Path::new("/workspace"),
        SandboxPermissions::UseDefault,
        ApprovalScope::Project,
        ApprovalSensitivityTier::Pattern,
    );

    assert_ne!(strict_session, directory_session);
    assert_ne!(directory_session, pattern_project);
    assert_ne!(strict_session, pattern_project);
}

#[test]
fn denied_approval_decisions_are_not_cached() {
    let mut store = ApprovalStore::default();
    store.put("denied".to_string(), ReviewDecision::Denied);

    assert_eq!(store.get("denied"), None);
}

#[test]
fn approval_store_revocation_removes_active_memo() {
    let now = Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap();
    let mut store = ApprovalStore::default();
    store.put_ttl(
        "key".to_string(),
        ReviewDecision::ApprovedForTtl,
        ApprovalScope::Project,
        ApprovalSensitivityTier::Directory,
        now,
        Duration::from_secs(300),
    );

    assert!(store.revoke("key"));
    assert_eq!(store.get_at("key", now), None);
    assert!(!store.revoke("key"));
}

#[test]
fn approval_store_allow_all_can_be_revoked_per_scope() {
    let mut store = ApprovalStore::default();
    store.approve_all_commands_for_scope("automation:turn-1");
    store.approve_all_commands();

    assert!(store.allow_all_commands());
    assert!(store.allow_all_commands_for_scope("automation:turn-1"));
    assert_eq!(store.active_allow_all_scopes().len(), 2);

    assert!(store.revoke_allow_all_for_scope("automation:turn-1"));
    assert!(!store.allow_all_commands_for_scope("automation:turn-1"));
    assert!(store.allow_all_commands(), "default scope should remain");
    assert!(!store.revoke_allow_all_for_scope("automation:turn-1"));
}

#[test]
fn approval_store_overflowing_ttl_falls_back_to_one_week_cap() {
    let now = Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap();
    let mut store = ApprovalStore::default();
    // Pathological caller passes a TTL that overflows chrono::Duration's
    // wall-clock arithmetic — the memo must still expire via the 7-day
    // fallback rather than silently becoming session-permanent.
    store.put_ttl(
        "huge".to_string(),
        ReviewDecision::ApprovedForTtl,
        ApprovalScope::Session,
        ApprovalSensitivityTier::Strict,
        now,
        Duration::MAX,
    );

    let inside_cap = now + chrono::Duration::hours(1);
    let beyond_cap = now + chrono::Duration::days(7) + chrono::Duration::seconds(1);
    assert!(
        store.get_at("huge", inside_cap).is_some(),
        "TTL fallback memo must remain active well within the cap"
    );
    assert_eq!(
        store.get_at("huge", beyond_cap),
        None,
        "TTL fallback must not exceed the 7-day safety cap"
    );
}

#[test]
fn approval_store_honest_long_ttl_is_not_silently_capped() {
    let now = Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap();
    let mut store = ApprovalStore::default();
    // A user-configured 14-day TTL is within chrono::Duration's range and
    // must be honoured — only the overflow path falls back to the 7-day cap.
    store.put_ttl(
        "long".to_string(),
        ReviewDecision::ApprovedForTtl,
        ApprovalScope::Session,
        ApprovalSensitivityTier::Strict,
        now,
        Duration::from_secs(60 * 60 * 24 * 14),
    );

    let twelve_days = now + chrono::Duration::days(12);
    assert!(
        store.get_at("long", twelve_days).is_some(),
        "honest 14-day TTL must remain active at 12 days, not silently expire at 7"
    );
}

#[tokio::test]
async fn ttl_approval_skips_second_prompt_within_ttl() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(Mutex::new(ApprovalStore::default()));
    let ctx = ToolApprovalContext {
        policy: AskForApproval::OnRequest,
        request_tx: tx,
        store: Arc::clone(&store),
        scope_key_prefix: None,
        approval_ttl: Duration::from_secs(60),
        cache_policy: ApprovalCachePolicy::default(),
    };
    let keys = vec![shell_approval_key(
        "cargo test --features test-provider --all-targets",
        Path::new("/workspace"),
        SandboxPermissions::UseDefault,
    )];

    let responder = tokio::spawn(async move {
        let request = rx.recv().await.expect("approval request expected");
        let _ = request.response_tx.send(ReviewDecision::ApprovedForTtl);
        rx
    });

    let first = request_cached_approval_with_keys(&ctx, &keys, |response_tx| {
        test_approval_request(response_tx)
    })
    .await;
    assert_eq!(first, ReviewDecision::ApprovedForTtl);

    let mut rx = responder.await.expect("responder task failed");
    let second = request_cached_approval_with_keys(&ctx, &keys, |response_tx| {
        test_approval_request(response_tx)
    })
    .await;

    assert_eq!(second, ReviewDecision::ApprovedForTtl);
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
}

fn test_approval_request(
    response_tx: tokio::sync::oneshot::Sender<ReviewDecision>,
) -> ExecApprovalRequest {
    ExecApprovalRequest {
        call_id: "call-ttl".to_string(),
        command: "cargo test --all-targets --features test-provider".to_string(),
        cwd: PathBuf::from("/workspace"),
        reason: None,
        is_retry: false,
        sandbox_label: "workspace-write".to_string(),
        network_access: NetworkAccess::Denied,
        writable_roots: vec![PathBuf::from("/workspace")],
        cache_disabled_reason: None,
        response_tx,
    }
}

/// plan-20260715 W4-07: Always approvals key on canonical `libra.repoid`.
#[tokio::test]
async fn approved_permission_project_id_is_canonical_repo_identity() {
    use libra::internal::{
        ai::permission::{ApprovalProvenance, ApprovedRulesetStore},
        db::migration::run_builtin_migrations,
    };
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    let conn = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        ["canonical-repo-id".into()],
    ))
    .await
    .expect("seed libra.repoid");

    ApprovedRulesetStore::append(
        &conn,
        "edit",
        "src/**",
        &ApprovalProvenance {
            source_worktree_id: "wt-linked".into(),
            source_session_id: "sess-1".into(),
            source_workspace_id: "ws-1".into(),
        },
    )
    .await
    .expect("append");

    let row = conn
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT project_id, source_worktree_id, source_session_id, source_workspace_id \
             FROM approved_permission"
                .to_string(),
        ))
        .await
        .expect("query")
        .expect("row");
    let project_id: String = row.try_get_by_index(0).expect("project_id");
    let wt: String = row.try_get_by_index(1).expect("wt");
    let sess: String = row.try_get_by_index(2).expect("sess");
    let ws: String = row.try_get_by_index(3).expect("ws");
    assert_eq!(project_id, "canonical-repo-id");
    assert_eq!(wt, "wt-linked");
    assert_eq!(sess, "sess-1");
    assert_eq!(ws, "ws-1");

    let loaded = ApprovedRulesetStore::load(&conn).await.expect("load");
    assert_eq!(loaded.project_id, "canonical-repo-id");
    assert_eq!(loaded.rules.len(), 1);
}

/// plan-20260715 W4-07: legacy opaque project_id rows stay invisible until
/// an explicit doctor adopt (no silent migration merge).
#[tokio::test]
async fn legacy_project_id_rows_require_doctor_adopt() {
    use libra::internal::{
        ai::permission::{ApprovalProvenance, ApprovedRulesetStore},
        db::migration::run_builtin_migrations,
    };
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    let conn = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        ["canonical-repo-id".into()],
    ))
    .await
    .expect("seed libra.repoid");

    ApprovedRulesetStore::append_for_project_id(
        &conn,
        "opaque-legacy",
        "shell",
        "rm -rf /",
        &ApprovalProvenance::empty(),
    )
    .await
    .expect("seed legacy");

    assert!(
        ApprovedRulesetStore::load(&conn)
            .await
            .expect("load")
            .is_empty(),
        "legacy project_id must not feed the runtime ruleset"
    );
    assert_eq!(
        ApprovedRulesetStore::list_legacy_project_ids(&conn)
            .await
            .expect("list"),
        vec!["opaque-legacy".to_string()]
    );

    ApprovedRulesetStore::adopt_legacy_project_id(&conn, "opaque-legacy")
        .await
        .expect("doctor adopt");

    let loaded = ApprovedRulesetStore::load(&conn)
        .await
        .expect("load after adopt");
    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(loaded.rules[0].permission, "shell");
    assert!(
        ApprovedRulesetStore::list_legacy_project_ids(&conn)
            .await
            .expect("list after adopt")
            .is_empty()
    );
}

fn run_libra(args: &[&str], cwd: &Path) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run libra");
    assert!(
        output.status.success(),
        "libra {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// plan-20260715 W4-13: Always approvals are repo_id-keyed and visible from
/// every linked worktree, with W4-07 provenance preserved.
#[tokio::test]
async fn always_approval_visible_across_worktrees_with_provenance() {
    let repo = tempfile::tempdir().expect("repo");
    setup_with_new_libra_in(repo.path()).await;
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    run_libra(&["add", "a.txt"], repo.path());
    run_libra(&["commit", "-m", "c1", "--no-verify"], repo.path());
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    run_libra(&["worktree", "add", wt.to_str().unwrap()], repo.path());

    let linked_scope = RequestScope::resolve(wt.clone()).expect("linked RequestScope");
    assert!(linked_scope.scope.is_linked());
    let wt_id = linked_scope.scope.storage_key().to_string();

    let db_path = linked_scope.storage.join(util::DATABASE);
    let conn = get_db_conn_instance_for_path(&db_path)
        .await
        .expect("open shared db");
    ApprovedRulesetStore::append(
        &conn,
        "edit",
        "src/**",
        &ApprovalProvenance {
            source_worktree_id: wt_id.clone(),
            source_session_id: "sess-linked".into(),
            source_workspace_id: "ws-1".into(),
        },
    )
    .await
    .expect("append from linked");

    let main_cache = resolve_approval_runtime_cache(repo.path())
        .await
        .expect("main cache");
    let linked_cache = resolve_approval_runtime_cache(&wt)
        .await
        .expect("linked cache");
    assert_eq!(main_cache.repo_id, linked_cache.repo_id);
    assert_eq!(main_cache.scope_key, linked_cache.scope_key);
    assert_eq!(
        main_cache.scope_key,
        approval_cache_scope_key(&main_cache.repo_id).expect("scope key")
    );
    assert_eq!(main_cache.approved_ruleset.rules.len(), 1);
    assert_eq!(linked_cache.approved_ruleset.rules.len(), 1);
    assert_eq!(
        main_cache.approved_ruleset.rules[0].permission,
        linked_cache.approved_ruleset.rules[0].permission
    );

    let from_main = ApprovedRulesetStore::list_with_provenance(&conn)
        .await
        .expect("provenance from shared db");
    assert_eq!(from_main.len(), 1);
    assert_eq!(from_main[0].provenance.source_worktree_id, wt_id);
    assert_eq!(from_main[0].provenance.source_session_id, "sess-linked");
    assert_eq!(from_main[0].provenance.source_workspace_id, "ws-1");
}

/// plan-20260715 W4-13: different repositories never share Always cache.
#[tokio::test]
async fn approved_permission_not_shared_across_repositories() {
    let repo_a = tempfile::tempdir().expect("repo a");
    let repo_b = tempfile::tempdir().expect("repo b");
    setup_with_new_libra_in(repo_a.path()).await;
    setup_with_new_libra_in(repo_b.path()).await;

    let conn_a = get_db_conn_instance_for_path(&repo_a.path().join(".libra").join(util::DATABASE))
        .await
        .expect("db a");
    ApprovedRulesetStore::append(&conn_a, "shell", "cargo test", &ApprovalProvenance::empty())
        .await
        .expect("append a");

    let cache_a = resolve_approval_runtime_cache(repo_a.path())
        .await
        .expect("cache a reload");
    let cache_b = resolve_approval_runtime_cache(repo_b.path())
        .await
        .expect("cache b");
    assert_ne!(cache_a.repo_id, cache_b.repo_id);
    assert_ne!(cache_a.scope_key, cache_b.scope_key);
    assert_eq!(cache_a.approved_ruleset.rules.len(), 1);
    assert!(
        cache_b.approved_ruleset.is_empty(),
        "Always approvals must not leak across repositories"
    );
}

/// plan-20260715 W4-13: session memos and pending interactions die on takeover.
#[tokio::test]
async fn session_approval_not_reused_after_lease_takeover() {
    let store = Arc::new(Mutex::new(ApprovalStore::default()));
    store.lock().await.put(
        "repo:example:session-shell".to_string(),
        ReviewDecision::ApprovedForSession,
    );
    assert_eq!(
        store.lock().await.get("repo:example:session-shell"),
        Some(ReviewDecision::ApprovedForSession)
    );

    let audit_sink = Arc::new(InMemoryAuditSink::default());
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "w4-13".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        audit_sink,
    );
    let (handle, worker) = AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(
        Arc::new(ExternalTurnTrackingExecutor),
        boundary,
    ));
    handle
        .track_external_turn(
            TurnRequest::new("session", "turn-1", "needs approval", false),
            tokio_util::sync::CancellationToken::new(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("track turn");
    handle
        .register_interaction(
            "session",
            "turn-1",
            InteractionState::AwaitingToolApproval {
                interaction_id: "approve-1".to_string(),
                tool_name: "shell".to_string(),
            },
        )
        .await
        .expect("pending approval");

    let session = CodeUiSession::new(initial_snapshot(
        "/tmp/w4-13",
        CodeUiProviderInfo {
            provider: "test".to_string(),
            model: Some("test-model".to_string()),
            mode: Some("web".to_string()),
            managed: false,
        },
        CodeUiCapabilities::default(),
    ));
    session
        .upsert_interaction(CodeUiInteractionRequest {
            id: "approve-1".to_string(),
            kind: CodeUiInteractionKind::Approval,
            title: Some("Approval".to_string()),
            description: None,
            prompt: None,
            options: Vec::new(),
            status: CodeUiInteractionStatus::Pending,
            metadata: serde_json::json!({}),
            requested_at: Utc::now(),
            resolved_at: None,
        })
        .await;
    session
        .set_status(CodeUiSessionStatus::AwaitingInteraction)
        .await;
    let adapter = AgentRuntimeCodeUiAdapter::new(
        session.clone(),
        CodeUiCapabilities::default(),
        handle.clone(),
        "session",
        Arc::new(ExecutionControlService::new("session", None, None).expect("execution control")),
        None,
        None,
    );
    adapter.set_approval_store(store.clone()).await;

    let mut options = CodeUiRuntimeOptions::new(true, false, CodeUiInitialController::Unclaimed);
    options.lease_duration = Some(chrono::Duration::milliseconds(80));
    let runtime = CodeUiRuntimeHandle::build_with_options(adapter, options).await;

    let first = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.attach_browser_controller("browser-a"),
    )
    .await
    .expect("first attach timed out")
    .expect("first attach");
    tokio::time::sleep(Duration::from_millis(120)).await;
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.attach_browser_controller("browser-b"),
    )
    .await
    .expect("takeover attach timed out")
    .expect("takeover attach");
    assert_ne!(first.controller_token, second.controller_token);

    assert_eq!(
        store.lock().await.get("repo:example:session-shell"),
        None,
        "session approval must not survive lease takeover"
    );
    let runtime_snapshot = handle
        .snapshot("session")
        .await
        .expect("runtime snapshot after takeover");
    assert!(
        !matches!(
            runtime_snapshot.interaction,
            InteractionState::AwaitingToolApproval { .. }
                | InteractionState::AwaitingUserInput { .. }
        ),
        "pending runtime interaction must not survive lease takeover: {:?}",
        runtime_snapshot.interaction
    );
    let respond = tokio::time::timeout(
        Duration::from_secs(2),
        handle.respond(
            "session",
            "turn-1",
            InteractionResponse::new("approve-1", "approved"),
        ),
    )
    .await
    .expect("respond after takeover timed out");
    assert!(
        respond.is_err(),
        "pending approval interaction must not be reusable after takeover: {respond:?}"
    );
    let snapshot = session.snapshot().await;
    assert!(
        snapshot.interactions.iter().all(|interaction| {
            interaction.id != "approve-1" || interaction.status != CodeUiInteractionStatus::Pending
        }),
        "stale Code UI approval prompt must not survive lease takeover: {:?}",
        snapshot.interactions
    );
    worker.abort();
}
