//! Live pipeline unit tests (ADR-ACF-10, ACF-19), moved from the hook
//! runtime. The deadline and scope-proof classification tests that exercise
//! only the hook entry stay in `hooks::runtime`.

use serial_test::serial;

use super::*;
#[cfg(unix)]
use crate::internal::ai::capture::key::load_capture_dedup_secret;
use crate::internal::ai::{
    capture::{
        ingress::{CaptureIngressBinding, CaptureRuntimeScope},
        live::{
            LeaseDisposition, LiveExportRunner, LiveExportTarget,
            hash_kind_from_object_format_lookup,
        },
        live_checkpoint::{
            ExportStageExit, attach_subagent_snapshot_metadata, build_extraction_metadata,
            export_stage_lease_disposition, omit_subagent_sources_for_deadline,
            snapshot_subagent_sources_for_extraction, subagent_extraction_warnings,
            terminal_snapshot_artifact_eligible,
        },
        snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService},
    },
    hooks::providers::claude_provider,
};

/// Isolate provider-root lookup for source-fence fixtures.  These tests
/// mutate a process-global test root, so Drop—not the happy-path tail of
/// an async test—must restore it before the serial env lane is released.
struct TestHomeGuard {
    prior: Option<std::ffi::OsString>,
}

impl TestHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let prior = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: test-only process environment mutation, restored by
        // Drop; every caller holds the serial env lane.
        unsafe {
            std::env::set_var("LIBRA_TEST_HOME", path);
        }
        Self { prior }
    }
}

impl Drop for TestHomeGuard {
    fn drop(&mut self) {
        // SAFETY: paired with `set`; unwind-safe restoration prevents a
        // failing source-fence assertion from leaking into a later test.
        unsafe {
            match &self.prior {
                Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                None => std::env::remove_var("LIBRA_TEST_HOME"),
            }
        }
    }
}

fn redacted_fixture(value: &str) -> crate::internal::ai::observed_agents::RedactedBytes {
    crate::internal::ai::observed_agents::Redactor::new_default()
        .redact(value.as_bytes())
        .0
}

/// Read the commitment elected by the real runtime from its durable
/// receipt. The snapshot service's helper checksum is intentionally only
/// transient, so source-fence tests must never predict this value with a
/// standalone snapshot call.
fn durable_terminal_source_commitment(metadata: &serde_json::Value) -> String {
    metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .and_then(|entries| entries.first())
        .and_then(|entry| entry["finalizer"]["source_digest"].as_str())
        .filter(|digest| {
            digest.strip_prefix("source/hmac-v2/").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
        })
        .expect("real runtime must durably bind a repository-keyed source commitment")
        .to_string()
}

/// B3-00: config read errors fail closed; missing key stays sha1; blake3 parses.
#[test]
fn runtime_config_read_error_fail_closed() {
    let blake3 = hash_kind_from_object_format_lookup(Ok(Some("blake3".to_string())))
        .expect("blake3 accepted via object_format helper");
    assert_eq!(blake3, git_internal::hash::HashKind::Blake3);

    let missing =
        hash_kind_from_object_format_lookup(Ok(None)).expect("missing key defaults to sha1");
    assert_eq!(missing, git_internal::hash::HashKind::Sha1);

    let err = hash_kind_from_object_format_lookup(Err(anyhow!("simulated config read failure")));
    let message = format!("{:#}", err.expect_err("Err must propagate"));
    assert!(
        message.contains("failed to read core.objectformat"),
        "expected contextual fail-closed message, got: {message}"
    );
    assert!(
        message.contains("simulated config read failure"),
        "expected underlying cause preserved, got: {message}"
    );

    let bad = hash_kind_from_object_format_lookup(Ok(Some("SHA256".to_string())));
    assert!(
        bad.is_err(),
        "mixed-case objectformat must fail closed via parse_config_value"
    );
}

/// AG-21 metadata persistence (codex review R2 P1): the generic E6
/// path (codex/opencode) must persist `subagent_token_usage` and
/// `api_call_count` into the checkpoint metadata `extraction` block —
/// not just into the in-memory summary. Drives the exact
/// `build_extraction_metadata` path a checkpoint write uses.
#[test]
fn extraction_metadata_persists_generic_subagent_and_api_count() {
    let transcript = redacted_fixture(concat!(
        r#"{"role":"user","content":"/review"}"#,
        "\n",
        r#"{"model":"gpt-5.3-codex","usage":{"input_tokens":10,"output_tokens":4,"api_call_count":5,"subagent_tokens":30}}"#,
        "\n",
    ));
    let value = build_extraction_metadata("codex", Some(&transcript), &[], &[]);
    let extraction = value.as_object().expect("extraction object");
    assert_eq!(extraction["present"], serde_json::json!(true));
    assert_eq!(
        extraction["api_call_count"],
        serde_json::json!(5),
        "wire api_call_count persisted (not +1): {value}"
    );
    let subagent = &extraction["subagent_token_usage"];
    assert_eq!(
        subagent["input_tokens"],
        serde_json::json!(30),
        "generic-path subagent tokens persisted: {value}"
    );

    // Claude uses the multi-source SubagentAwareExtractor accessor. A
    // parent Task marker alone must not become attributed usage; only an
    // independently supplied child transcript contributes that field.
    let claude_line = serde_json::json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "model": "claude-sonnet-5",
            "content": [{"type": "tool_use", "name": "Task", "input": {"prompt": "x"}}],
            "usage": {"input_tokens": 7, "output_tokens": 2}
        }
    });
    let parent = redacted_fixture(&format!("{claude_line}\n"));
    let without_child = build_extraction_metadata("claude_code", Some(&parent), &[], &[]);
    assert!(
        without_child.get("subagent_token_usage").is_none(),
        "parent total is not subagent attribution: {without_child}"
    );
    let child = redacted_fixture(concat!(
        r#"{"type":"assistant","message":{"role":"assistant","content":"child","usage":{"input_tokens":3,"output_tokens":1}}}"#,
        "\n",
    ));
    let claude = build_extraction_metadata(
        "claude_code",
        Some(&parent),
        std::slice::from_ref(&child),
        &[],
    );
    assert!(
        claude["subagent_token_usage"].is_object(),
        "child transcript usage present via accessor: {claude}"
    );
    assert_eq!(claude["subagent_source_count"], serde_json::json!(1));

    let child_only =
        build_extraction_metadata("claude_code", None, std::slice::from_ref(&child), &[]);
    assert_eq!(child_only["present"], serde_json::json!(true));
    assert_eq!(child_only["partial"], serde_json::json!(true));
    assert_eq!(
        child_only["subagent_source_count"],
        serde_json::json!(1),
        "durable child content remains attributable without parent bytes: {child_only}"
    );
    assert_eq!(
        child_only["subagent_token_usage"]["input_tokens"],
        serde_json::json!(3),
        "child-only usage remains visible: {child_only}"
    );
    assert!(
        child_only["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("child sources only")))),
        "child-only extraction records why the aggregate is partial: {child_only}"
    );
}

#[test]
fn subagent_snapshot_redacts_child_before_parent_extraction() {
    let secret = format!("ghp_{}", "a".repeat(36));
    let child_payload = format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":"{secret}","usage":{{"input_tokens":3,"output_tokens":1}}}}}}"#
    );
    let child = crate::internal::ai::subagent_content::DiscoveredSubagentContent::fixture(
        "claude_code",
        "source/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        child_payload.as_bytes(),
        None,
    );
    let snapshots = snapshot_subagent_sources_for_extraction(
        vec![child],
        "claude_code__parent",
        CaptureSnapshotPolicy::default(),
    );
    assert_eq!(snapshots.transcripts.len(), 1);
    assert_eq!(snapshots.partial_source_count(), 0);
    assert_eq!(
        snapshots.projections[0]
            .source
            .as_ref()
            .expect("subagent source projection")
            .kind,
        crate::internal::ai::capture::snapshot::CaptureSnapshotSourceKind::DiscoveredSubagent
    );
    assert!(
        !String::from_utf8_lossy(snapshots.transcripts[0].bytes()).contains(&secret),
        "child secret must be scrubbed before extraction"
    );

    let extraction = build_extraction_metadata(
        "claude_code",
        None,
        &snapshots.transcripts,
        &subagent_extraction_warnings(None, snapshots.partial_source_count()),
    );
    assert_eq!(
        extraction["subagent_token_usage"]["input_tokens"],
        serde_json::json!(3),
        "redacted child still contributes usage attribution: {extraction}"
    );
    assert!(
        !serde_json::to_string(&extraction)
            .expect("serialize extraction")
            .contains(&secret),
        "secret must not reappear through derived extraction metadata"
    );
}

#[test]
fn partial_subagent_snapshot_is_omitted_and_parent_metadata_stays_safe() {
    let secret = format!("ghp_{}", "b".repeat(36));
    let child = crate::internal::ai::subagent_content::DiscoveredSubagentContent::fixture(
        "claude_code",
        "source/sha256/abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        secret.as_bytes(),
        None,
    );
    let snapshots = snapshot_subagent_sources_for_extraction(
        vec![child],
        "claude_code__parent",
        CaptureSnapshotPolicy::with_deadline(Some(std::time::Instant::now())),
    );
    assert!(snapshots.transcripts.is_empty());
    assert_eq!(snapshots.partial_source_count(), 1);
    let warnings = subagent_extraction_warnings(None, snapshots.partial_source_count());
    let mut extraction = build_extraction_metadata("claude_code", None, &[], &warnings);
    attach_subagent_snapshot_metadata(&mut extraction, &snapshots);
    assert_eq!(extraction["partial"], serde_json::json!(true));
    assert_eq!(
        extraction["subagent_snapshot"]["partial_source_count"],
        serde_json::json!(1)
    );
    assert!(
        !serde_json::to_string(&extraction)
            .expect("serialize partial extraction")
            .contains(&secret),
        "unsafe child bytes must not become a partial-metadata fallback"
    );
}

/// A managed SessionEnd deadline deliberately does not route discovered
/// native child bytes through the synchronous snapshot/redaction chain.
/// The helper gets only the parent's redacted bytes and an explicit
/// partial flag, so a malformed or sensitive child can never extend the
/// hook's CPU work or leak through the safe status projection.
#[test]
fn deadline_extraction_omits_discovered_children_without_parent_parsing() {
    let secret = format!("ghp_{}", "c".repeat(36));
    let child = crate::internal::ai::subagent_content::DiscoveredSubagentContent::fixture(
        "claude_code",
        "source/sha256/abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd",
        format!("not-json-and-never-redacted:{secret}").as_bytes(),
        None,
    );
    let snapshots = omit_subagent_sources_for_deadline(vec![child]);
    assert!(snapshots.transcripts.is_empty());
    assert!(snapshots.projections.is_empty());
    assert!(snapshots.redaction_reports.is_empty());
    assert_eq!(snapshots.deadline_omitted_source_count, 1);

    let mut extraction = CaptureSnapshotService::deadline_extraction_partial(true);
    attach_subagent_snapshot_metadata(&mut extraction, &snapshots);
    assert_eq!(extraction["partial"], serde_json::json!(true));
    assert_eq!(
        extraction["subagent_snapshot"]["deadline_omitted_source_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        extraction["subagent_snapshot"]["partial_reasons"][0],
        serde_json::json!("deadline_exceeded")
    );
    assert!(
        !serde_json::to_string(&extraction)
            .expect("serialize deadline extraction status")
            .contains(&secret),
        "deadline child omission must not parse, redact, or persist native child bytes"
    );
}

// Scenario: identical native session IDs from different providers do not
// collide because the namespacing prefix differs.
#[test]
fn session_id_is_namespaced_by_provider() {
    assert_eq!(
        build_ai_session_id("gemini", "session-123"),
        "gemini__session-123"
    );
    assert_eq!(
        build_ai_session_id("claude", "session-123"),
        "claude__session-123"
    );
}

// Scenario: no provider or native session-identity fragment reaches logs.
#[test]
#[serial_test::serial(cwd, hash_kind)]
fn session_id_redaction_masks_all_identity() {
    assert_eq!(redact_session_id("gemini__session-123"), "***");
    assert_eq!(redact_session_id("short"), "***");
    let codex_pi_identity = "codex__/pi__native-session-secret";
    let rendered = redact_session_id(codex_pi_identity);
    assert_eq!(rendered, "***");
    assert!(
        !rendered.contains("codex")
            && !rendered.contains("pi__")
            && !rendered.contains("native-session-secret"),
        "no provider/native session identity fragment may reach telemetry"
    );
}

// -------------------------------------------------------------------
// CEX-EntireIO: AgentTraces ingest tests. Codex round-2 BLOCK #10 + #2
// round-3 followup ("assert observable redaction outcome").
// -------------------------------------------------------------------

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, ExecResult, Statement,
};
use tempfile::TempDir;

use crate::internal::db::{ensure_ai_runtime_contract_schema, migration::run_builtin_migrations};

const LEGACY_BOOTSTRAP_SQL: &str = include_str!("../../../../sql/sqlite_20260309_init.sql");

pub(crate) async fn ingest_fresh_conn() -> (TempDir, DatabaseConnection) {
    let dir = tempfile::tempdir().expect("tempdir");
    // Use the canonical `libra.db` filename here so the Phase 3.5c
    // object_index queue (`enqueue_agent_blob_object_index_update`)
    // — which derives the database path from `repo_path.join(DATABASE)`
    // — finds the same file the test fixture set up.
    let path = dir.path().join(crate::utils::util::DATABASE);
    std::fs::File::create(&path).expect("touch sqlite file");
    let url = format!("sqlite://{}", path.display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    let conn = Database::connect(opts).await.expect("connect");
    // Mirror production wiring exactly: legacy bootstrap (creates
    // `ai_thread`) → AI runtime contract → registered migrations.
    let backend = conn.get_database_backend();
    for raw in LEGACY_BOOTSTRAP_SQL.split(';') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _: ExecResult = conn
            .execute_raw(Statement::from_string(backend, trimmed.to_string()))
            .await
            .unwrap_or_else(|e| panic!("legacy bootstrap stmt failed: {trimmed}\n{e}"));
    }
    ensure_ai_runtime_contract_schema(&conn)
        .await
        .expect("ensure_ai_runtime_contract_schema");
    run_builtin_migrations(&conn)
        .await
        .expect("run_builtin_migrations");
    // Production repositories carry `libra.repoid` from `libra init`, and
    // the ingest path resolves it through `CaptureScope` — so a fixture
    // without it fails identity resolution before any behaviour under
    // test runs. Mint it the way production does for a legacy repository.
    crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
        .await
        .expect("seed libra.repoid for the test repository");
    (dir, conn)
}

pub(crate) fn ingest_envelope(
    hook_event_name: &str,
    session_id: &str,
    extra: serde_json::Value,
) -> Vec<u8> {
    let mut base = json!({
        "hook_event_name": hook_event_name,
        "session_id": session_id,
        "cwd": "/tmp/repo",
        "transcript_path": "/tmp/repo/transcript.jsonl",
    });
    if let serde_json::Value::Object(extra_map) = extra
        && let serde_json::Value::Object(base_map) = &mut base
    {
        for (k, v) in extra_map {
            base_map.insert(k, v);
        }
    }
    serde_json::to_vec(&base).expect("serialize envelope")
}

/// Unit-test-only convenience wrapper. Raw test frames remain owned by
/// `capture::ingress`; the production runtime has no raw payload
/// ingress API.
async fn ingest_agent_traces_payload(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    conn: &sea_orm::DatabaseConnection,
    repo_path: Option<&std::path::Path>,
) -> Result<()> {
    let outcome = crate::internal::ai::capture::ingress::lower_in_process_capture_frame_for_test(
        payload,
        command,
        expected_kind,
        provider,
        repo_path,
    );
    crate::internal::ai::capture::test_support::ingest_agent_traces_ingress_outcome_for_test(
        outcome, command, provider, conn, repo_path,
    )
    .await
}

struct StableScopeIngestOptions<'a> {
    stable_repo_path: &'a std::path::Path,
    checkpoint_repo_path: Option<&'a std::path::Path>,
    deadline: Option<CaptureDeadline>,
    /// `None` resolves the provider's own binding, as the hook entry does.
    binding: Option<LiveCaptureBinding>,
}

/// Drive the typed runtime with a stable verified worktree while choosing
/// whether a checkpoint store is available. Production binds the
/// worktree before ingress; this helper models a transiently unavailable
/// checkpoint backend without changing the receipt's session identity.
async fn ingest_agent_traces_payload_with_stable_scope(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    conn: &sea_orm::DatabaseConnection,
    stable_repo_path: &std::path::Path,
    checkpoint_repo_path: Option<&std::path::Path>,
) -> Result<()> {
    ingest_agent_traces_payload_with_stable_scope_and_deadline(
        payload,
        command,
        expected_kind,
        provider,
        conn,
        StableScopeIngestOptions {
            stable_repo_path,
            checkpoint_repo_path,
            deadline: None,
            binding: None,
        },
    )
    .await
}

async fn ingest_agent_traces_payload_with_stable_scope_and_deadline(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    conn: &sea_orm::DatabaseConnection,
    options: StableScopeIngestOptions<'_>,
) -> Result<()> {
    ingest_agent_traces_payload_with_stable_scope_and_lease(
        payload,
        command,
        expected_kind,
        provider,
        conn,
        options,
        &ExportJobLeaseStore::new(conn),
    )
    .await
}

/// [`ingest_agent_traces_payload_with_stable_scope_and_deadline`] with an
/// injected export-job lease port (and, through the options, an optional
/// pre-resolved binding), so export-stage tests can observe every lease
/// settlement.
async fn ingest_agent_traces_payload_with_stable_scope_and_lease(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    conn: &sea_orm::DatabaseConnection,
    options: StableScopeIngestOptions<'_>,
    export_lease: &impl LiveExportLeasePort,
) -> Result<()> {
    let verified_cwd = options
        .stable_repo_path
        .to_str()
        .context("test repository path is not valid UTF-8")?
        .to_string();
    let outcome = CaptureIngressCommand::from_payload(
        payload,
        command,
        expected_kind,
        provider,
        options.deadline,
        |_| {
            Ok(CaptureIngressBinding::new(
                verified_cwd,
                [0xA5; 32],
                CaptureRuntimeScope::new(
                    options.stable_repo_path.to_path_buf(),
                    options.stable_repo_path.to_path_buf(),
                ),
            ))
        },
    )?;
    let CaptureIngressOutcome::Command(command) = outcome else {
        bail!("test fixture unexpectedly lowered to an unknown lifecycle event");
    };
    let scope = CaptureScope::main_for_connection(conn).await?;
    let ingest_span = new_ingest_span(command.hook_command(), provider);
    ingest_agent_traces_payload_with_scope(
        command,
        options
            .binding
            .unwrap_or_else(|| LiveCaptureBinding::resolve(provider)),
        conn,
        options.checkpoint_repo_path,
        &scope,
        &ingest_span,
        export_lease,
    )
    .await
}

/// A managed AgentTraces pure read is cancellable, unlike catalog DML or
/// COMMIT acknowledgement.  Hold a real file SQLite EXCLUSIVE lock on a
/// second connection so the first `sqlite_master` preflight read proves
/// that cancellation cannot turn into a delayed session publication once
/// the holder releases its lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_traces_read_deadline_bounds_exclusive_sqlite_lock_without_delayed_mutation() {
    let (dir, conn) = ingest_fresh_conn().await;
    let database_path = dir.path().join(crate::utils::util::DATABASE);
    let database_path = database_path
        .to_str()
        .expect("managed-read deadline database path is utf-8");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "PRAGMA journal_mode=DELETE",
    ))
    .await
    .expect("force rollback journal for exclusive read contention");
    let reader = crate::internal::db::establish_connection_with_busy_timeout(
        database_path,
        Duration::from_secs(1),
    )
    .await
    .expect("open independent managed-read contender");
    let locker = crate::internal::db::establish_connection_with_busy_timeout(
        database_path,
        Duration::from_secs(1),
    )
    .await
    .expect("open independent managed-read lock holder");
    let repo_path = dir.path().to_path_buf();
    let scope = CaptureScope::main_for_connection(&reader)
        .await
        .expect("resolve stable main scope before contention");
    let locker_backend = locker.get_database_backend();
    locker
        .execute_raw(Statement::from_string(locker_backend, "BEGIN EXCLUSIVE"))
        .await
        .expect("acquire second-connection exclusive SQLite lock");

    // Establish the paired deadline only once the real lock is held, so
    // the regression cannot pass from an earlier fixture setup delay.
    let deadline =
        CaptureDeadline::from_budget_millis(30).expect("construct short managed-read deadline");
    let outcome = CaptureIngressCommand::from_payload(
        &ingest_envelope(
            "SessionStart",
            "S-managed-read-deadline",
            json!({"prompt": "must not persist after a cancelled pure read"}),
        ),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        Some(deadline),
        |_| {
            Ok(CaptureIngressBinding::new(
                repo_path
                    .to_str()
                    .expect("stable repository path is utf-8")
                    .to_string(),
                [0xA5; 32],
                CaptureRuntimeScope::new(repo_path.clone(), repo_path.clone()),
            ))
        },
    )
    .expect("lower managed-read deadline fixture");
    let CaptureIngressOutcome::Command(command) = outcome else {
        panic!("managed-read deadline fixture must lower to a command");
    };
    let ingest_span = new_ingest_span(command.hook_command(), claude_provider());

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        ingest_agent_traces_payload_with_scope(
            command,
            LiveCaptureBinding::resolve(claude_provider()),
            &reader,
            None,
            &scope,
            &ingest_span,
            &ExportJobLeaseStore::new(&reader),
        ),
    )
    .await;
    locker
        .execute_raw(Statement::from_string(locker_backend, "ROLLBACK"))
        .await
        .expect("release second-connection exclusive SQLite lock");

    let error = result
        .expect("managed pure read must stop at its short deadline")
        .expect_err("exclusive lock must reject the managed pure read");
    assert!(
        is_agent_traces_read_deadline(&error),
        "exclusive-read expiry must retain the typed deadline error: {error:#}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a 30ms pure-read deadline must not wait for the contender's one-second SQLite busy timeout"
    );

    // If dropping the timed-out SQLx future left a queued statement that
    // later resumed, this pause/recheck would observe an unauthorized
    // SessionStart after the holder releases its lock.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let backend = conn.get_database_backend();
    let sessions = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_session WHERE provider_session_id = ?",
            ["S-managed-read-deadline".into()],
        ))
        .await
        .expect("count sessions after cancelled managed read")
        .expect("session count row");
    assert_eq!(
        sessions
            .try_get_by::<i64, _>("n")
            .expect("decode session count"),
        0,
        "a timed-out preflight read must not publish a delayed agent session"
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint",
            [],
        ))
        .await
        .expect("count checkpoints after cancelled managed read")
        .expect("checkpoint count row");
    assert_eq!(
        checkpoints
            .try_get_by::<i64, _>("n")
            .expect("decode checkpoint count"),
        0,
        "a timed-out preflight read must not create a checkpoint"
    );
}

#[tokio::test]
async fn ingest_session_start_creates_active_row() {
    let (_dir, conn) = ingest_fresh_conn().await;

    let payload = ingest_envelope("SessionStart", "S-001", json!({}));
    ingest_agent_traces_payload(
        &payload,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session start ingest succeeds");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT agent_kind, state, working_dir, provider_session_id, stopped_at \
                 FROM agent_session WHERE provider_session_id = ?",
            ["S-001".into()],
        ))
        .await
        .expect("query")
        .expect("session row exists");

    assert_eq!(
        row.try_get_by::<String, _>("agent_kind").unwrap(),
        "claude_code"
    );
    assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "active");
    assert_eq!(
        row.try_get_by::<String, _>("working_dir").unwrap(),
        "/in-process-capture",
        "the in-process ingress seam must persist its verified binding, not the \
             envelope-controlled cwd"
    );
    assert_eq!(
        row.try_get_by::<String, _>("provider_session_id").unwrap(),
        "S-001"
    );
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none()
    );
}

fn fixture_subagent_discovery() -> crate::internal::ai::subagent_content::SubagentDiscovery {
    crate::internal::ai::subagent_content::SubagentDiscovery {
        sources: vec![
            crate::internal::ai::subagent_content::DiscoveredSubagentContent::fixture(
                "claude_code",
                concat!(
                    "source/sha256/",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                ),
                br#"{"type":"user","uuid":"child-user","message":{"role":"user","content":"child"}}
"#,
                None,
            ),
        ],
        bytes_read: 78,
        warning: None,
        incomplete: false,
    }
}

async fn checkpoint_scope_count(conn: &DatabaseConnection, scope: &str) -> i64 {
    conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT COUNT(*) AS count FROM agent_checkpoint WHERE scope = ?",
        [scope.into()],
    ))
    .await
    .expect("count checkpoint scope")
    .expect("checkpoint scope count row")
    .try_get_by("count")
    .expect("decode checkpoint scope count")
}

#[tokio::test]
async fn in_process_subagent_content_fault_keeps_child_evidence_before_parent_checkpoint() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let storage_path = repo_path.join(".libra");
    std::fs::create_dir_all(storage_path.join("objects"))
        .expect("create subagent content object store");
    std::fs::hard_link(
        repo_path.join(crate::utils::util::DATABASE),
        storage_path.join(crate::utils::util::DATABASE),
    )
    .expect("expose the fixture database through its canonical storage path");
    assert_eq!(
        crate::utils::util::try_get_storage_path(Some(storage_path.clone()))
            .expect("subagent fixture repository storage"),
        storage_path,
        "the in-process fixture must expose the same storage root to the child capability"
    );
    let session = "S-subagent-post-content-fault";
    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", session, json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&storage_path),
    )
    .await
    .expect("start subagent post-content fault fixture");

    let _direct_child_writer = test_support::capture_subagent_without_deadline();
    test_support::fail_after_subagent_content_before_parent_checkpoint_once();
    let result = crate::internal::ai::subagent_content::with_subagent_discovery_override(
        fixture_subagent_discovery(),
        ingest_agent_traces_payload(
            &ingest_envelope(
                "SessionEnd",
                session,
                json!({"event_id":"subagent-post-content-fault-v1"}),
            ),
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&storage_path),
        ),
    )
    .await;
    let error =
        result.expect_err("the in-process fault must interrupt after durable child capture");
    assert!(
        format!("{error:#}").contains("injected failure after subagent content"),
        "the injected post-content fault, not child capture setup, must fail: {error:#}"
    );
    assert_eq!(
        checkpoint_scope_count(&conn, "subagent").await,
        1,
        "child content must be durable before the injected parent failure"
    );
    assert_eq!(
        checkpoint_scope_count(&conn, "committed").await,
        0,
        "the parent checkpoint must not advertise child attribution before it commits"
    );

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT state, stopped_at, metadata_json FROM agent_session \
                 WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("read interrupted terminal session")
        .expect("interrupted terminal session exists");
    assert_eq!(
        row.try_get_by::<String, _>("state")
            .expect("decode interrupted terminal state"),
        "active",
        "a failed parent append must not publish the terminal session state"
    );
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .expect("decode interrupted terminal timestamp")
            .is_none(),
        "a failed parent append must not advertise a terminal timestamp"
    );
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode interrupted receipt metadata"),
    )
    .expect("receipt metadata is JSON");
    assert!(
        metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["status"] == "pending")),
        "the failed terminal delivery must remain durably pending for replay: {metadata}"
    );

    let replay = crate::internal::ai::subagent_content::with_subagent_discovery_override(
        fixture_subagent_discovery(),
        ingest_agent_traces_payload(
            &ingest_envelope(
                "SessionEnd",
                session,
                json!({"event_id":"subagent-post-content-fault-v1"}),
            ),
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&storage_path),
        ),
    )
    .await;
    replay.expect("the pending terminal receipt must replay after the child leaf is durable");
    assert_eq!(
        checkpoint_scope_count(&conn, "subagent").await,
        1,
        "replay must reuse the durable child leaf rather than append a duplicate"
    );
    assert_eq!(
        checkpoint_scope_count(&conn, "committed").await,
        1,
        "replay must publish exactly one parent checkpoint"
    );
    let replayed = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT state, stopped_at, metadata_json FROM agent_session \
                 WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("read replayed terminal session")
        .expect("replayed terminal session exists");
    assert_eq!(
        replayed
            .try_get_by::<String, _>("state")
            .expect("decode replayed terminal state"),
        "stopped",
        "only the replayed durable parent append may publish terminal state"
    );
    let replayed_metadata: serde_json::Value = serde_json::from_str(
        &replayed
            .try_get_by::<String, _>("metadata_json")
            .expect("decode replayed receipt metadata"),
    )
    .expect("replayed receipt metadata is JSON");
    assert!(
        replayed_metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["status"] == "complete")),
        "the replay must complete its terminal receipt: {replayed_metadata}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn in_process_subagent_discovery_deadline_preserves_partial_parent_checkpoint() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-subagent-parent-preservation";
    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", session, json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start subagent deadline fixture");

    let helper = dir.path().join("stalled-subagent-discovery.sh");
    std::fs::write(&helper, "#!/bin/sh\nexec sleep 30\n")
        .expect("write stalled subagent discovery helper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&helper)
            .expect("read stalled helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)
            .expect("make stalled subagent discovery helper executable");
    }
    let _deadline = test_support::override_subagent_discovery_deadline(80);
    let result = crate::internal::ai::subagent_content::with_subagent_discovery_helper_program(
        helper,
        ingest_agent_traces_payload(
            &ingest_envelope(
                "SessionEnd",
                session,
                json!({"event_id":"subagent-parent-preservation-v1"}),
            ),
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        ),
    )
    .await;
    result.expect("a discovery deadline must preserve a partial parent checkpoint");
    assert_eq!(
        checkpoint_scope_count(&conn, "subagent").await,
        0,
        "expired discovery must not publish unvalidated child content"
    );
    assert_eq!(
        checkpoint_scope_count(&conn, "committed").await,
        1,
        "the parent checkpoint remains durable and explicitly partial"
    );
}

#[tokio::test]
async fn ingest_tombstone_blocks_stale_hook_session_resurrection() {
    let (_dir, conn) = ingest_fresh_conn().await;
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_import_tombstone (
                tombstone_id, agent_kind, provider_session_id,
                erased_session_id, erased_at
             ) VALUES (?, 'claude_code', ?, ?, ?)",
        [
            "tombstone-stale-hook".into(),
            "S-erased".into(),
            "claude__S-erased".into(),
            1_i64.into(),
        ],
    ))
    .await
    .expect("seed erasure tombstone");

    let payload = ingest_envelope("SessionStart", "S-erased", json!({}));
    let err = ingest_agent_traces_payload(
        &payload,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect_err("stale hook must not resurrect an erased session");
    // Catalog failures retain their sanitized actionable cause through
    // the coordinator boundary, so a stale hook is distinguishable from
    // an ordinary scope-claim conflict.
    assert!(
        err.to_string()
            .contains("tombstoned and cannot be recreated by a stale hook"),
        "unexpected error: {err:#}"
    );

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_session WHERE provider_session_id = ?",
            ["S-erased".into()],
        ))
        .await
        .expect("query session count")
        .expect("count row");
    assert_eq!(row.try_get_by::<i64, _>("n").expect("decode count"), 0);
}

/// The catalog reservation is authoritative only while its row remains
/// durable. If erasure wins immediately afterwards, the defensive owner
/// confirmation must fail closed instead of treating a missing row as our
/// own claim and publishing a checkpoint from stale source state.
#[tokio::test]
async fn erased_reservation_cannot_fabricate_a_checkpoint_owner() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-erased-after-reservation";
    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", session, json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("establish checkpoint-capable session");

    crate::internal::ai::capture::test_support::delete_reserved_session_once(
        // Arm the post-reservation erase for this hook's Libra session id.
        build_ai_session_id("claude", session),
    );
    let error = ingest_agent_traces_payload(
        &ingest_envelope(
            "Stop",
            session,
            json!({"event_id": "erased-reservation-v1"}),
        ),
        ProviderHookCommand::Stop,
        LifecycleEventKind::TurnEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect_err("a deleted reserved row must not become a self-owned checkpoint writer");
    assert!(
        format!("{error:#}").contains("disappeared after its catalog reservation"),
        "unexpected post-reservation error: {error:#}"
    );

    let backend = conn.get_database_backend();
    for (sql, label) in [
        (
            "SELECT COUNT(*) AS n FROM agent_checkpoint",
            "checkpoint catalog row",
        ),
        (
            "SELECT COUNT(*) AS n FROM reference WHERE name = 'libra/traces'",
            "traces ref",
        ),
        (
            "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'",
            "writer marker",
        ),
    ] {
        let row = conn
            .query_one_raw(Statement::from_string(backend, sql.to_string()))
            .await
            .expect("read erased-reservation side-effect count")
            .expect("count row");
        assert_eq!(
            row.try_get_by::<i64, _>("n")
                .expect("decode side-effect count"),
            0,
            "erased reservation must not create a {label}"
        );
    }
}

#[tokio::test]
async fn fresh_hook_session_consumes_saved_capture_incarnation() {
    let (_dir, conn) = ingest_fresh_conn().await;
    let backend = conn.get_database_backend();
    let namespace = "0123456789abcdef0123456789abcdef";
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_capture_incarnation (
                agent_kind, provider_session_id, next_session_sync_revision,
                source_namespace, updated_at
             ) VALUES ('claude_code', ?, 7, ?, 1)",
        ["S-restored-live".into(), namespace.into()],
    ))
    .await
    .expect("seed erased-session replication incarnation");

    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", "S-restored-live", json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("fresh live hook consumes the saved incarnation");

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT sync_revision,
                        json_extract(metadata_json, '$.capture_incarnation') AS incarnation
                 FROM agent_session WHERE provider_session_id = ?",
            ["S-restored-live".into()],
        ))
        .await
        .expect("query restored live session")
        .expect("restored live session row");
    assert_eq!(row.try_get_by::<i64, _>("sync_revision").unwrap(), 7);
    assert_eq!(
        row.try_get_by::<String, _>("incarnation").unwrap(),
        namespace
    );
}

#[tokio::test]
async fn ingest_session_end_without_checkpoint_keeps_terminal_receipt_pending() {
    let (_dir, conn) = ingest_fresh_conn().await;

    let start = ingest_envelope("SessionStart", "S-002", json!({}));
    ingest_agent_traces_payload(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("start ok");

    // A native identity makes this a true replay of the same terminal
    // action rather than a later independent SessionEnd delivery.
    let end = ingest_envelope("SessionEnd", "S-002", json!({"event_id": "end-002"}));
    ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("end ok");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, sync_revision FROM agent_session
                 WHERE provider_session_id = ?",
            ["S-002".into()],
        ))
        .await
        .expect("query")
        .expect("row");

    assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "active");
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        row.try_get_by::<i64, _>("sync_revision").unwrap(),
        2,
        "terminal reservation advances the monotonic generation once"
    );

    // Same pending receipt resumes without another state mutation while
    // there is no checkpoint facade available to complete it.
    ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("pending terminal replay is accepted");

    // Repeat-ingest is idempotent: still exactly one row and the same
    // reserved revision for that session.
    let count_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_session WHERE provider_session_id = ?",
            ["S-002".into()],
        ))
        .await
        .expect("count query")
        .expect("count row");
    assert_eq!(count_row.try_get_by::<i64, _>("n").unwrap(), 1);
    let replay_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, sync_revision FROM agent_session \
                 WHERE provider_session_id = ?",
            ["S-002".into()],
        ))
        .await
        .expect("replay query")
        .expect("replay row");
    assert_eq!(
        replay_row.try_get_by::<String, _>("state").unwrap(),
        "active"
    );
    assert!(
        replay_row
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        replay_row.try_get_by::<i64, _>("sync_revision").unwrap(),
        2,
        "ResumePending must not repeat the terminal reservation"
    );
}

/// Drive the typed runtime with a validated hook cwd that may differ from
/// the repository root, as a real producer does when the agent changes
/// directory inside the worktree mid-session.
async fn ingest_with_verified_cwd(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    conn: &sea_orm::DatabaseConnection,
    verified_cwd: &std::path::Path,
    repo_root: &std::path::Path,
) -> Result<()> {
    let verified_cwd = verified_cwd
        .to_str()
        .context("test hook cwd is not valid UTF-8")?
        .to_string();
    let outcome = CaptureIngressCommand::from_payload(
        payload,
        command,
        expected_kind,
        claude_provider(),
        None,
        |_| {
            Ok(CaptureIngressBinding::new(
                verified_cwd,
                [0xA5; 32],
                CaptureRuntimeScope::new(repo_root.to_path_buf(), repo_root.to_path_buf()),
            ))
        },
    )?;
    let CaptureIngressOutcome::Command(command) = outcome else {
        bail!("test fixture unexpectedly lowered to an unknown lifecycle event");
    };
    let scope = CaptureScope::main_for_connection(conn).await?;
    let ingest_span = new_ingest_span(command.hook_command(), claude_provider());
    ingest_agent_traces_payload_with_scope(
        command,
        LiveCaptureBinding::resolve(claude_provider()),
        conn,
        Some(repo_root),
        &scope,
        &ingest_span,
        &ExportJobLeaseStore::new(conn),
    )
    .await
}

/// ACF-13 VER1: a validated SessionEnd whose hook cwd differs from the
/// catalog working_dir cannot be recorded, so it must end explicitly
/// incomplete (non-zero, content-free) rather than be acknowledged. It
/// leaves no receipt, checkpoint, artifact row, or worker hint, and the
/// session row is byte-identical. A nonterminal event from the same cwd
/// stays advisory.
#[tokio::test]
async fn hook_cwd_outside_catalog_working_dir_leaves_no_replayable_artifact() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_root = dir
        .path()
        .canonicalize()
        .expect("canonicalize moved-cwd test repository");
    std::fs::create_dir_all(repo_root.join("objects"))
        .expect("create moved-cwd fixture object storage");
    let moved_cwd = repo_root.join("nested-hook-cwd");
    std::fs::create_dir_all(&moved_cwd).expect("create nested hook cwd");
    let provider_session = "S-moved-hook-cwd";
    let backend = conn.get_database_backend();

    ingest_with_verified_cwd(
        &ingest_envelope("SessionStart", provider_session, json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        &conn,
        &repo_root,
        &repo_root,
    )
    .await
    .expect("session starts at the repository root");
    let session_row = || async {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT state, stopped_at, sync_revision, working_dir, metadata_json \
                     FROM agent_session WHERE provider_session_id = ?",
                [provider_session.into()],
            ))
            .await
            .expect("query moved-cwd session")
            .expect("moved-cwd session row");
        (
            row.try_get_by::<String, _>("state").unwrap(),
            row.try_get_by::<Option<i64>, _>("stopped_at").unwrap(),
            row.try_get_by::<i64, _>("sync_revision").unwrap(),
            row.try_get_by::<String, _>("working_dir").unwrap(),
            row.try_get_by::<String, _>("metadata_json").unwrap(),
        )
    };
    let before = session_row().await;
    assert_eq!(before.0, "active");
    assert_eq!(before.3, repo_root.to_string_lossy());

    ingest_with_verified_cwd(
        &ingest_envelope("Stop", provider_session, json!({"event_id": "moved-stop"})),
        ProviderHookCommand::Stop,
        LifecycleEventKind::TurnEnd,
        &conn,
        &moved_cwd,
        &repo_root,
    )
    .await
    .expect("a nonterminal identity conflict stays advisory");
    assert_eq!(session_row().await, before);

    let error = ingest_with_verified_cwd(
        &ingest_envelope(
            "SessionEnd",
            provider_session,
            json!({"event_id": "moved-session-end"}),
        ),
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        &conn,
        &moved_cwd,
        &repo_root,
    )
    .await
    .expect_err("a terminal identity conflict must not be silently acknowledged");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("terminal boundary is incomplete")
            && rendered.contains("no checkpoint or recovery artifact was retained"),
        "terminal conflict must be an explicit incomplete outcome: {rendered}"
    );
    for secret in [
        provider_session.to_string(),
        repo_root.to_string_lossy().to_string(),
        "nested-hook-cwd".to_string(),
    ] {
        assert!(
            !rendered.contains(&secret),
            "terminal conflict diagnostic must stay content-free: {rendered}"
        );
    }

    assert_eq!(
        session_row().await,
        before,
        "the refused terminal must not stop, revise, or add a receipt to the session"
    );
    let count = |sql: &'static str| {
        let conn = &conn;
        async move {
            conn.query_one_raw(Statement::from_string(backend, sql.to_string()))
                .await
                .expect("count moved-cwd evidence")
                .expect("count row")
                .try_get_by::<i64, _>("n")
                .unwrap()
        }
    };
    assert_eq!(
        count("SELECT COUNT(*) AS n FROM agent_checkpoint").await,
        0,
        "no checkpoint may be published for the refused terminal"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope IN \
                 ('agent_capture_pending', 'agent_capture_pending_chunk', \
                  'agent_capture_quarantine')"
        )
        .await,
        0,
        "no replayable artifact may be retained for the refused terminal"
    );
    let scope = CaptureScope::main_for_connection(&conn)
        .await
        .expect("resolve moved-cwd capture scope");
    assert!(
        !crate::internal::ai::capture::pending::has_pending_hint(&conn, &scope)
            .await
            .expect("inspect recovery hint"),
        "a later SessionStart must find no hint, so no recovery worker launches"
    );
}

/// ACF-13 F2: only a live-channel terminal snapshot becomes a replayable
/// artifact. An OpenCode export-channel SessionEnd holds an export lease
/// and export-owned claims that the artifact codec cannot carry; it must
/// stay on the synchronous checkpoint path instead of failing at seal.
#[test]
fn terminal_artifact_branch_is_limited_to_live_channel_snapshots() {
    assert!(terminal_snapshot_artifact_eligible(
        true, true, "live", false
    ));
    assert!(!terminal_snapshot_artifact_eligible(
        true, true, "export", true
    ));
    assert!(!terminal_snapshot_artifact_eligible(
        true, true, "export", false
    ));
    assert!(!terminal_snapshot_artifact_eligible(
        true, true, "live", true
    ));
    assert!(!terminal_snapshot_artifact_eligible(
        true, false, "live", false
    ));
    assert!(!terminal_snapshot_artifact_eligible(
        false, true, "live", false
    ));
}

#[tokio::test]
async fn expired_host_budget_still_persists_a_terminal_pending_receipt() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-expired-host-terminal";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("session start succeeds");

    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"prompt": "terminal content must not enter the expired recovery path"}),
    );
    // The test needs an expired primary pair, not an arbitrary historical
    // wall value: SQLite final authorization now correctly rejects a
    // literal 2023 deadline. Keep both clocks just expired so the fixed
    // terminal settlement slice remains available.
    let original_deadline_millis = Utc::now().timestamp_millis() - 1;
    let expired_deadline = CaptureDeadline::from_parts(
        Instant::now() - Duration::from_millis(1),
        original_deadline_millis,
    );
    ingest_agent_traces_payload_with_stable_scope_and_deadline(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        StableScopeIngestOptions {
            stable_repo_path: &repo_path,
            checkpoint_repo_path: None,
            deadline: Some(expired_deadline),
            binding: None,
        },
    )
    .await
    .expect("expired terminal host budget must settle as pending, not drop the event");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query expired terminal row")
        .expect("expired terminal row exists");
    assert_ne!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none()
    );
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode expired terminal metadata"),
    )
    .expect("expired terminal metadata JSON");
    let finalizer = &metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"];
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["status"],
        json!("pending")
    );
    assert_eq!(
        finalizer["deadline_millis"],
        json!(original_deadline_millis),
        "runtime must preserve the original dispatch-time wall deadline rather than re-anchor it: {metadata}"
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count expired-terminal checkpoints")
        .expect("expired-terminal checkpoint count row");
    assert_eq!(
        checkpoints.try_get_by::<i64, _>("n").unwrap(),
        0,
        "the expired fast path must leave a pending receipt before any checkpoint work"
    );
}

#[tokio::test]
async fn terminal_deadline_elapsed_after_ingress_still_settles_before_checkpoint_work() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-terminal-deadline-after-ingress";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("start deadline-transition fixture");

    let _delay = test_support::delay_ingress_before_redaction(50);
    let deadline = CaptureDeadline::from_budget_millis(20)
        .expect("establish paired ingress transition deadline");
    let deadline_millis = deadline.absolute_millis();
    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"prompt": "this content must be discarded after the deadline transition"}),
    );
    ingest_agent_traces_payload_with_stable_scope_and_deadline(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        StableScopeIngestOptions {
            stable_repo_path: &repo_path,
            checkpoint_repo_path: Some(&repo_path),
            deadline: Some(deadline),
            binding: None,
        },
    )
    .await
    .expect("late terminal deadline must settle a receipt instead of starting checkpoint work");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query transitioned terminal row")
        .expect("transitioned terminal row exists");
    assert_ne!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode transitioned terminal metadata"),
    )
    .expect("transitioned terminal metadata JSON");
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["deadline_millis"],
        json!(deadline_millis),
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count transitioned-terminal checkpoints")
        .expect("transitioned-terminal checkpoint count row");
    assert_eq!(
        checkpoints.try_get_by::<i64, _>("n").unwrap(),
        0,
        "the post-ingress deadline transition must skip snapshot/checkpoint work"
    );
}

#[tokio::test]
async fn terminal_deadline_after_primary_reservation_resumes_only_pending_finalizer() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-terminal-finalizer-handoff-deadline";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("start finalizer-handoff fixture");

    // The delay is deliberately after the primary lifecycle reservation:
    // the recovery route must not let the old reservation's expired port
    // strand a terminal receipt without finalizer evidence.
    let deadline = CaptureDeadline::from_budget_millis(20)
        .expect("establish paired terminal finalizer handoff deadline");
    let deadline_millis = deadline.absolute_millis();
    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"prompt": "must not reach source work after finalizer handoff deadline"}),
    );
    let result = test_support::with_terminal_finalizer_after_reservation_delay(
        Duration::from_millis(60),
        ingest_agent_traces_payload_with_stable_scope_and_deadline(
            &end,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            StableScopeIngestOptions {
                stable_repo_path: &repo_path,
                checkpoint_repo_path: Some(&repo_path),
                deadline: Some(deadline),
                binding: None,
            },
        ),
    )
    .await;
    result.expect("elapsed post-reservation terminal must retain a pending finalizer");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, sync_revision, metadata_json
                 FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query finalizer-handoff terminal row")
        .expect("finalizer-handoff terminal row exists");
    assert_ne!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none(),
        "the grace route may only retain a pending receipt"
    );
    assert_eq!(
        row.try_get_by::<i64, _>("sync_revision").unwrap(),
        2,
        "the settlement resume must not reapply the primary lifecycle mutation"
    );
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode finalizer-handoff metadata"),
    )
    .expect("finalizer-handoff metadata JSON");
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["status"],
        json!("pending"),
    );
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["deadline_millis"],
        json!(deadline_millis),
        "the receipt retains the primary dispatch deadline rather than the grace deadline"
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count finalizer-handoff checkpoints")
        .expect("finalizer-handoff checkpoint count row");
    assert_eq!(
        checkpoints.try_get_by::<i64, _>("n").unwrap(),
        0,
        "the settlement handoff cannot start checkpoint work"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn terminal_deadline_after_live_coverage_reservation_abandons_claim_and_stays_pending() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir
        .path()
        .canonicalize()
        .expect("canonicalize coverage-deadline test repository");
    std::fs::create_dir_all(repo_path.join("objects"))
        .expect("create coverage-deadline fixture object storage");
    load_capture_dedup_secret(&repo_path)
        .expect("seed coverage-deadline fixture source commitment key");
    let home = tempfile::tempdir().expect("test Claude home");
    let _home_guard = TestHomeGuard::set(home.path());
    let session = "S-terminal-coverage-deadline";
    let transcript_dir = crate::internal::ai::observed_agents::claude_session_dir(&repo_path)
        .expect("resolve test Claude transcript root");
    std::fs::create_dir_all(&transcript_dir).expect("create test Claude transcript root");
    std::fs::write(
        transcript_dir.join(format!("{session}.jsonl")),
        concat!(
            r#"{"type":"user","uuid":"coverage-deadline-u1","message":{"role":"user","content":"finish bounded capture"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"coverage-deadline-a1","message":{"role":"assistant","content":[{"type":"text","text":"coverage reservation must not outlive terminal deadline"}]}}"#,
            "\n",
        ),
    )
    .expect("write coverage-deadline transcript");

    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("start coverage-deadline fixture");

    // The unit binary cannot dispatch the production private helper arg,
    // but this still reads the real provider-derived source rather than
    // injecting a snapshot. The outer deadline remains active for the
    // coverage reservation and finalizer path under test.
    let _source_boundary = test_support::capture_live_without_deadline();
    // Cross the primary deadline after the reservation, but remain within
    // the fixed 250ms terminal settlement slice.
    let _delay = test_support::delay_live_coverage_after_reservation(3_020);
    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "coverage-deadline-terminal-v1"}),
    );
    let deadline = CaptureDeadline::from_budget_millis(3_000)
        .expect("establish paired post-reservation deadline");
    let deadline_millis = deadline.absolute_millis();
    ingest_agent_traces_payload_with_stable_scope_and_deadline(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        StableScopeIngestOptions {
            stable_repo_path: &repo_path,
            checkpoint_repo_path: Some(&repo_path),
            deadline: Some(deadline),
            binding: None,
        },
    )
    .await
    .expect("expired post-reservation terminal must retain a pending receipt");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query post-reservation deadline row")
        .expect("post-reservation deadline row exists");
    assert_ne!(
        row.try_get_by::<String, _>("state").unwrap(),
        "stopped",
        "a terminal deadline after reservation must not publish completion"
    );
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none(),
        "a deadline-expired terminal receipt remains replayable"
    );
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode post-reservation deadline metadata"),
    )
    .expect("post-reservation deadline metadata JSON");
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["status"],
        json!("pending"),
        "the finalizer must stay pending after its coverage deadline"
    );
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["deadline_millis"],
        json!(deadline_millis),
        "the pending finalizer keeps the original wall deadline"
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count post-reservation deadline checkpoints")
        .expect("post-reservation deadline checkpoint count row");
    assert_eq!(
        checkpoints.try_get_by::<i64, _>("n").unwrap(),
        0,
        "no checkpoint may be appended after the capture deadline"
    );
    let claim = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, owner FROM agent_coverage_claim
                 WHERE session_id = (SELECT session_id FROM agent_session
                                     WHERE provider_session_id = ? LIMIT 1)
                   AND logical_turn_key = 'coverage-deadline-u1'",
            [session.into()],
        ))
        .await
        .expect("query released coverage claim")
        .expect("coverage reservation was acquired before the injected deadline");
    assert_eq!(
        claim.try_get_by::<String, _>("state").unwrap(),
        "abandoned",
        "a terminal deadline must release its live coverage reservation"
    );
    assert!(
        claim
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none(),
        "released coverage claims cannot retain the expired writer owner"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn terminal_deadline_during_live_coverage_reservation_settles_pending() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir
        .path()
        .canonicalize()
        .expect("canonicalize reservation-deadline test repository");
    std::fs::create_dir_all(repo_path.join("objects"))
        .expect("create reservation-deadline fixture object storage");
    load_capture_dedup_secret(&repo_path)
        .expect("seed reservation-deadline fixture source commitment key");
    let home = tempfile::tempdir().expect("test Claude home");
    let _home_guard = TestHomeGuard::set(home.path());
    let session = "S-terminal-coverage-reservation-deadline";
    let transcript_dir = crate::internal::ai::observed_agents::claude_session_dir(&repo_path)
        .expect("resolve reservation-deadline Claude transcript root");
    std::fs::create_dir_all(&transcript_dir)
        .expect("create reservation-deadline Claude transcript root");
    std::fs::write(
        transcript_dir.join(format!("{session}.jsonl")),
        concat!(
            r#"{"type":"user","uuid":"coverage-reservation-deadline-u1","message":{"role":"user","content":"finish bounded capture"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"coverage-reservation-deadline-a1","message":{"role":"assistant","content":[{"type":"text","text":"a reservation deadline must keep the receipt pending"}]}}"#,
            "\n",
        ),
    )
    .expect("write reservation-deadline transcript");

    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("start reservation-deadline fixture");

    // This preserves a real source read while targeting the error path
    // from the atomic claim transaction itself (rather than the later
    // post-reservation deadline check).
    let _source_boundary = test_support::capture_live_without_deadline();
    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "coverage-reservation-deadline-terminal-v1"}),
    );
    let deadline = CaptureDeadline::from_budget_millis(1_000)
        .expect("establish paired live-coverage reservation deadline");
    let deadline_millis = deadline.absolute_millis();
    let result = crate::internal::ai::coverage_gate::with_live_reservation_turn_delay(
        // The reservation itself passes the primary deadline but the
        // content-free terminal settlement remains within its fixed grace
        // window.
        Duration::from_millis(1_100),
        ingest_agent_traces_payload_with_stable_scope_and_deadline(
            &end,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            StableScopeIngestOptions {
                stable_repo_path: &repo_path,
                checkpoint_repo_path: Some(&repo_path),
                deadline: Some(deadline),
                binding: None,
            },
        ),
    )
    .await;
    result.expect(
        "an elapsed coverage-transaction deadline must settle the terminal receipt pending",
    );

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query reservation-deadline terminal row")
        .expect("reservation-deadline terminal row exists");
    assert_ne!(
        row.try_get_by::<String, _>("state").unwrap(),
        "stopped",
        "a reservation error at an elapsed deadline must not publish completion"
    );
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none(),
        "the terminal receipt remains replayable after its reservation deadline"
    );
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode reservation-deadline metadata"),
    )
    .expect("reservation-deadline metadata JSON");
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["status"],
        json!("pending"),
        "the deadline-error branch must retain a pending finalizer"
    );
    assert_eq!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["deadline_millis"],
        json!(deadline_millis),
        "the pending finalizer keeps its original wall deadline"
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count reservation-deadline checkpoints")
        .expect("reservation-deadline checkpoint count row");
    assert_eq!(
        checkpoints.try_get_by::<i64, _>("n").unwrap(),
        0,
        "a deadline-error reservation must not append a checkpoint"
    );
    let claims = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = (SELECT session_id FROM agent_session
                                     WHERE provider_session_id = ? LIMIT 1)
                   AND logical_turn_key = 'coverage-reservation-deadline-u1'",
            [session.into()],
        ))
        .await
        .expect("count rolled-back reservation claims")
        .expect("rolled-back reservation claim count row");
    assert_eq!(
        claims.try_get_by::<i64, _>("n").unwrap(),
        0,
        "the deadline-aborted atomic reservation must not leave a claim behind"
    );
}

/// A terminal callback can arrive before the hook has a repository path.
/// That first delivery persists only an explicitly unbound finalizer
/// receipt. When the *same native delivery* is replayed after a repository
/// becomes available, the first real checkpoint must bind that provisional
/// receipt before writing; treating it like an ordinary pending marker
/// would quarantine the replay as a false generation takeover.
#[tokio::test]
async fn no_repository_terminal_replay_binds_unbound_finalizer_before_checkpoint() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-unbound-terminal-replay";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("session start without a repository checkpoint succeeds");

    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "unbound-terminal-replay-v1"}),
    );
    ingest_agent_traces_payload_with_stable_scope(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("no-repository terminal delivery persists an unbound receipt");

    ingest_agent_traces_payload_with_stable_scope(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        &repo_path,
        Some(&repo_path),
    )
    .await
    .expect("the same terminal delivery binds and completes exactly once");

    let backend = conn.get_database_backend();
    let terminal = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query terminal state")
        .expect("session row exists");
    let terminal_metadata: String = terminal.try_get_by("metadata_json").unwrap();
    assert_eq!(
        terminal.try_get_by::<String, _>("state").unwrap(),
        "stopped",
        "a real checkpoint replay must complete rather than quarantine the provisional receipt: {terminal_metadata}"
    );
    assert!(
        terminal
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_some(),
        "terminal publication still requires the strict finalizer completion"
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count checkpoints")
        .expect("checkpoint count row");
    assert_eq!(
        checkpoints.try_get_by::<i64, _>("n").unwrap(),
        1,
        "binding an unbound receipt must create one deterministic checkpoint"
    );
}

#[tokio::test]
async fn expired_distinct_terminal_after_completion_does_not_strand_a_new_receipt() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-expired-durable-terminal";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("start durable-terminal fixture");
    let first_end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "durable-terminal-first"}),
    );
    ingest_agent_traces_payload_with_stable_scope(
        &first_end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        &repo_path,
        Some(&repo_path),
    )
    .await
    .expect("complete durable terminal fixture");

    let backend = conn.get_database_backend();
    let before = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("read completed terminal metadata")
        .expect("completed terminal row")
        .try_get_by::<String, _>("metadata_json")
        .expect("decode completed terminal metadata");

    let distinct_expired_end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "durable-terminal-distinct-expired"}),
    );
    ingest_agent_traces_payload_with_stable_scope_and_deadline(
        &distinct_expired_end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        StableScopeIngestOptions {
            stable_repo_path: &repo_path,
            checkpoint_repo_path: Some(&repo_path),
            deadline: Some(CaptureDeadline::from_parts(
                Instant::now() - Duration::from_millis(1),
                1_700_000_000_124,
            )),
            binding: None,
        },
    )
    .await
    .expect("expired duplicate of a complete terminal is a durable no-op");

    let after = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("read terminal after expired duplicate")
        .expect("terminal row remains") ;
    assert_eq!(after.try_get_by::<String, _>("state").unwrap(), "stopped");
    assert!(
        after
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        after.try_get_by::<String, _>("metadata_json").unwrap(),
        before,
        "an expired distinct delivery must not append an unrecoverable pending receipt after terminal completion"
    );
}

#[tokio::test]
async fn idless_no_repository_terminal_redelivery_reuses_the_pending_action() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-idless-terminal-replay";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("session start succeeds");

    // Claude may omit a provider-native event ID. The two calls below
    // therefore mint distinct ingress UUIDs; the second must adopt the
    // first local pending receipt instead of stranding its finalizer.
    let end = ingest_envelope("SessionEnd", session, json!({}));
    ingest_agent_traces_payload_with_stable_scope(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        &repo_path,
        None,
    )
    .await
    .expect("id-less terminal persists an unbound receipt");
    ingest_agent_traces_payload_with_stable_scope(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        &repo_path,
        Some(&repo_path),
    )
    .await
    .expect("id-less terminal redelivery adopts and completes the first receipt");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query terminal row")
        .expect("id-less terminal row exists");
    assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode id-less receipt ledger"),
    )
    .expect("id-less receipt metadata JSON");
    let entries = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .expect("receipt entries");
    assert_eq!(entries.len(), 1, "one terminal receipt must remain");
    assert_eq!(entries[0]["status"], json!("complete"));
}

/// ACF-04 replay regression: when the durable checkpoint lands but the
/// process fails before receipt completion, the identical SessionEnd
/// delivery must reuse the same checkpoint instead of appending a second
/// traces commit. It may then atomically publish the deferred terminal
/// state.
#[tokio::test]
#[serial(env)]
async fn session_end_retry_reuses_durable_checkpoint_before_receipt_completion() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let start = ingest_envelope("SessionStart", "S-catalog-replay", json!({}));
    ingest_agent_traces_payload(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start succeeds");

    let end = ingest_envelope(
        "SessionEnd",
        "S-catalog-replay",
        json!({"event_id": "catalog-replay-end-1"}),
    );
    // This typed, in-process seam exists only in the test build. Unlike
    // an environment knob it cannot be supplied by a hook host or alter
    // a production binary; it interrupts exactly after the durable
    // checkpoint write and before strict receipt completion.
    crate::internal::ai::capture::coordinator::test_support::interrupt_after_checkpoint_once();
    let first = ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await;
    assert!(
        first.is_err(),
        "test fault must leave a durable checkpoint with a pending receipt"
    );

    let backend = conn.get_database_backend();
    let before = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, traces_commit FROM agent_checkpoint \
                 WHERE session_id = (SELECT session_id FROM agent_session \
                   WHERE provider_session_id = ? LIMIT 1)",
            ["S-catalog-replay".into()],
        ))
        .await
        .expect("query durable checkpoint")
        .expect("checkpoint is durable before receipt completion");
    let first_checkpoint_id: String = before.try_get_by("checkpoint_id").unwrap();
    let first_traces_commit: String = before.try_get_by("traces_commit").unwrap();
    let pending = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at FROM agent_session WHERE provider_session_id = ?",
            ["S-catalog-replay".into()],
        ))
        .await
        .expect("query pending terminal state")
        .expect("pending terminal session exists");
    assert_eq!(pending.try_get_by::<String, _>("state").unwrap(), "active");
    assert!(
        pending
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none(),
        "SessionEnd must not publish terminal state before receipt completion"
    );

    ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("same pending SessionEnd resumes and completes");

    let after = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, traces_commit FROM agent_checkpoint \
                 WHERE session_id = (SELECT session_id FROM agent_session \
                   WHERE provider_session_id = ? LIMIT 1)",
            ["S-catalog-replay".into()],
        ))
        .await
        .expect("query replay checkpoint")
        .expect("replay checkpoint exists");
    assert_eq!(
        after.try_get_by::<String, _>("checkpoint_id").unwrap(),
        first_checkpoint_id,
        "retry must retain the original deterministic checkpoint ID"
    );
    assert_eq!(
        after.try_get_by::<String, _>("traces_commit").unwrap(),
        first_traces_commit,
        "retry must not append a second traces commit"
    );
    let count = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            ["S-catalog-replay".into()],
        ))
        .await
        .expect("count replay checkpoints")
        .expect("checkpoint count row");
    assert_eq!(count.try_get_by::<i64, _>("n").unwrap(), 1);
    let completed = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at FROM agent_session WHERE provider_session_id = ?",
            ["S-catalog-replay".into()],
        ))
        .await
        .expect("query completed terminal state")
        .expect("completed session exists");
    assert_eq!(
        completed.try_get_by::<String, _>("state").unwrap(),
        "stopped"
    );
    assert!(
        completed
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_some(),
        "receipt completion publishes stopped_at"
    );
}

/// ACF-04 changed-source replay regression: when source X has already
/// committed the deterministic terminal checkpoint and retired its marker
/// before receipt completion, source Y may finish only X's receipt. It
/// must not register Y, append a second checkpoint/ref, or replace the
/// persisted source fence.
#[tokio::test]
#[serial(env)]
async fn changed_source_after_durable_terminal_write_completes_elected_receipt_only() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir
        .path()
        .canonicalize()
        .expect("canonicalize durable-replay test repository");
    // The direct-storage fixture models a repository only once its object
    // directory exists. The HMAC capability deliberately rechecks this
    // binding before it can mint a durable source commitment.
    std::fs::create_dir_all(repo_path.join("objects"))
        .expect("create source-fence fixture object storage");
    let home = tempfile::tempdir().expect("test Claude home");
    let _home_guard = TestHomeGuard::set(home.path());

    let result = async {
        let session = "S-terminal-durable-source-replay";
        let transcript_dir =
            crate::internal::ai::observed_agents::claude_session_dir(&repo_path)
                .expect("test home resolves Claude source root");
        std::fs::create_dir_all(&transcript_dir).expect("create Claude transcript root");
        let transcript_path = transcript_dir.join(format!("{session}.jsonl"));
        let source_x = concat!(r#"{"type":"summary","marker":"source-x"}"#, "\n");
        std::fs::write(&transcript_path, source_x).expect("write elected source fixture");

        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", session, json!({})),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start durable-replay fixture");

        let terminal = ingest_envelope(
            "SessionEnd",
            session,
            json!({"event_id": "terminal-durable-source-replay-v1"}),
        );
        crate::internal::ai::capture::coordinator::test_support::interrupt_after_checkpoint_once();
        let first = ingest_agent_traces_payload(
            &terminal,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await;
        assert!(
            first.is_err(),
            "the injected post-checkpoint interruption must retain a pending receipt"
        );

        let backend = conn.get_database_backend();
        let before = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT s.state, s.stopped_at, s.metadata_json,
                            c.checkpoint_id, c.traces_commit,
                            (SELECT COUNT(*) FROM agent_checkpoint c2
                             WHERE c2.session_id = s.session_id) AS checkpoints,
                            (SELECT COUNT(*) FROM metadata_kv m
                             WHERE m.scope = 'agent_traces_inflight'
                               AND m.target = s.session_id) AS markers,
                            (SELECT r.\"commit\" FROM reference r
                             WHERE r.name = ? AND r.kind = 'Branch'
                               AND r.remote IS NULL AND r.\"commit\" IS NOT NULL
                             LIMIT 1) AS trace_head
                     FROM agent_session s
                     JOIN agent_checkpoint c ON c.session_id = s.session_id
                     WHERE s.provider_session_id = ?",
                [
                    crate::internal::branch::TRACES_BRANCH.into(),
                    session.into(),
                ],
            ))
            .await
            .expect("read durable terminal checkpoint")
            .expect("source X checkpoint is durable before completion");
        assert_eq!(
            before.try_get_by::<String, _>("state").expect("decode pending state"),
            "active",
            "terminal state cannot publish before strict receipt completion"
        );
        assert!(before
            .try_get_by::<Option<i64>, _>("stopped_at")
            .expect("decode pending stopped_at")
            .is_none());
        assert_eq!(
            before.try_get_by::<i64, _>("markers").expect("count retired markers"),
            0,
            "the ordinary terminal marker must be retired after durable write"
        );
        let first_checkpoint_id: String = before
            .try_get_by("checkpoint_id")
            .expect("decode source X checkpoint id");
        let first_traces_commit: String = before
            .try_get_by("traces_commit")
            .expect("decode source X traces commit");
        let first_trace_head: String = before
            .try_get_by("trace_head")
            .expect("decode source X traces ref");
        let before_metadata: serde_json::Value = serde_json::from_str(
            &before
                .try_get_by::<String, _>("metadata_json")
                .expect("decode pending receipt metadata"),
        )
        .expect("pending receipt metadata JSON");
        let source_x_commitment = durable_terminal_source_commitment(&before_metadata);
        assert_eq!(
            before_metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]
                ["source_digest"],
            json!(source_x_commitment),
            "the durable pending receipt must remain fenced to source X"
        );

        let source_y = concat!(r#"{"type":"summary","marker":"source-y"}"#, "\n");
        std::fs::write(&transcript_path, source_y).expect("write changed source fixture");
        assert_ne!(
            source_x, source_y,
            "the replay must exercise a genuinely changed authorized source"
        );

        ingest_agent_traces_payload(
            &terminal,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("changed source completes only source X's durable receipt");

        let after = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT s.state, s.stopped_at, s.metadata_json,
                            c.checkpoint_id, c.traces_commit,
                            (SELECT COUNT(*) FROM agent_checkpoint c2
                             WHERE c2.session_id = s.session_id) AS checkpoints,
                            (SELECT COUNT(*) FROM metadata_kv m
                             WHERE m.scope = 'agent_traces_inflight'
                               AND m.target = s.session_id) AS markers,
                            (SELECT r.\"commit\" FROM reference r
                             WHERE r.name = ? AND r.kind = 'Branch'
                               AND r.remote IS NULL AND r.\"commit\" IS NOT NULL
                             LIMIT 1) AS trace_head
                     FROM agent_session s
                     JOIN agent_checkpoint c ON c.session_id = s.session_id
                     WHERE s.provider_session_id = ?",
                [
                    crate::internal::branch::TRACES_BRANCH.into(),
                    session.into(),
                ],
            ))
            .await
            .expect("read completed durable replay")
            .expect("completed source X session exists");
        assert_eq!(after.try_get_by::<String, _>("state").expect("decode stopped state"), "stopped");
        assert!(after
            .try_get_by::<Option<i64>, _>("stopped_at")
            .expect("decode stopped timestamp")
            .is_some());
        assert_eq!(
            after.try_get_by::<String, _>("checkpoint_id").expect("decode replay checkpoint id"),
            first_checkpoint_id,
            "source Y must not create a checkpoint identity"
        );
        assert_eq!(
            after.try_get_by::<String, _>("traces_commit").expect("decode replay traces commit"),
            first_traces_commit,
            "source Y must not append another traces commit"
        );
        assert_eq!(
            after.try_get_by::<String, _>("trace_head").expect("decode replay trace ref"),
            first_trace_head,
            "source Y must not move refs/libra/traces"
        );
        assert_eq!(after.try_get_by::<i64, _>("checkpoints").expect("count checkpoints"), 1);
        assert_eq!(after.try_get_by::<i64, _>("markers").expect("count markers"), 0);
        let after_metadata: serde_json::Value = serde_json::from_str(
            &after
                .try_get_by::<String, _>("metadata_json")
                .expect("decode completed receipt metadata"),
        )
        .expect("completed receipt metadata JSON");
        assert_eq!(
            after_metadata["capture_catalog_receipts_v1"]["entries"][0]["status"],
            json!("complete")
        );
        assert_eq!(
            after_metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]
                ["source_digest"],
            json!(source_x_commitment),
            "the changed source must not replace source X's durable receipt fence"
        );
        assert_eq!(
            durable_terminal_source_commitment(&after_metadata),
            source_x_commitment,
            "the changed source must not replace source X's durable receipt fence"
        );

        Ok::<(), anyhow::Error>(())
    }
    .await;

    result.expect("changed-source durable terminal replay regression");
}

/// An admitted export runner owns its export-job lease before the
/// exporter's result is classified. Every source-stage rejection (an
/// impossible `File` output, a snapshot that loses its redacted bytes)
/// settles the runner token `Failed`; this drives that exact settlement
/// through the production lease store for each classification and proves no
/// `inflight` lease survives either fail-closed rejection.
#[tokio::test]
async fn opencode_rejected_runner_sources_release_export_leases() {
    let (_dir, conn) = ingest_fresh_conn().await;
    let scope = CaptureScope {
        repo_id: "runtime-opencode-export-rejection".to_string(),
        worktree_id: String::new(),
        workspace_id: None,
        workspace_fence: None,
    };
    let store = ExportJobLeaseStore::new(&conn);
    for (provider_session_id, rejection) in [
        ("opencode-unexpected-file", "unexpected File source"),
        ("opencode-missing-redacted", "missing redacted transcript"),
    ] {
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30))
            .expect("establish export runner deadline");
        let runner = LiveExportRunner::admit(
            &store,
            LiveExportTarget {
                agent_kind: "opencode",
                provider_session_id,
                scope: &scope,
            },
            deadline,
        )
        .await
        .expect("acquire export runner lease")
        .expect("fresh export job must elect this test as runner");
        let exit = ExportStageExit::SourceRejected(anyhow!("{rejection}"));
        let (disposition, reason, log) = export_stage_lease_disposition(&exit, provider_session_id);
        assert_eq!(disposition, LeaseDisposition::Failed, "{rejection}");
        runner
            .settle(disposition, reason, log)
            .await
            .expect("a failed-runner release never fails its caller");

        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, lease_expires_at, last_error_code
                     FROM agent_export_job
                     WHERE agent_kind = 'opencode' AND provider_session_id = ?",
                [provider_session_id.into()],
            ))
            .await
            .expect("read settled export job")
            .expect("settled export job exists");
        assert_eq!(
            row.try_get_by::<String, _>("state")
                .expect("decode settled job state"),
            "failed",
            "{rejection} must settle the acquired runner"
        );
        assert!(
            row.try_get_by::<Option<String>, _>("owner")
                .expect("decode settled job owner")
                .is_none(),
            "{rejection} may not leak an inflight owner"
        );
        assert!(
            row.try_get_by::<Option<i64>, _>("lease_expires_at")
                .expect("decode settled lease deadline")
                .is_none(),
            "{rejection} may not leave an inflight lease deadline"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("last_error_code")
                .expect("decode failed job code"),
            Some("LBR-AGENT-005".to_string()),
            "{rejection} remains actionable for a later idle/doctor"
        );
    }
}

/// A sibling terminal delivery can observe the elected marker while the
/// first writer is still before ref-CAS. It must return retryable failure
/// (not a false durability acknowledgement and not a finalizer retry),
/// after which the elected writer still commits exactly one checkpoint.
#[tokio::test]
async fn terminal_duplicate_inflight_is_not_acknowledged_before_durability() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-terminal-inflight";
    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", session, json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start terminal duplicate fixture");

    let terminal = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "terminal-inflight-v1"}),
    );
    let pause =
        crate::internal::ai::capture::checkpoint::test_support::pause_after_registration_once(
            &repo_path,
        );
    let first_conn = conn.clone();
    let first_repo = repo_path.clone();
    let first_terminal = terminal.clone();
    let first = tokio::spawn(async move {
        ingest_agent_traces_payload(
            &first_terminal,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &first_conn,
            Some(&first_repo),
        )
        .await
    });
    pause.wait_until_entered().await;

    let sibling = ingest_agent_traces_payload(
        &terminal,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect_err("duplicate terminal delivery must not acknowledge a live writer");
    assert!(
        format!("{sibling:#}").contains("still in flight"),
        "unexpected duplicate terminal error: {sibling:#}"
    );

    pause.release();
    first
        .await
        .expect("join elected terminal writer")
        .expect("elected terminal writer commits after pause");

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT state, stopped_at, metadata_json,
                    (SELECT COUNT(*) FROM agent_checkpoint c
                     WHERE c.session_id = agent_session.session_id) AS checkpoints
                 FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("read terminal duplicate result")
        .expect("terminal session remains durable");
    assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_some()
    );
    assert_eq!(row.try_get_by::<i64, _>("checkpoints").unwrap(), 1);
    let metadata: serde_json::Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode terminal receipt metadata"),
    )
    .expect("terminal receipt metadata JSON");
    assert_ne!(
        metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["status"],
        json!("quarantined"),
        "duplicate in-flight delivery must not consume finalizer retry budget"
    );
}

/// A later delivery of the same native terminal event can observe a
/// changed, still-authorized transcript after the first delivery has
/// already elected its source fence. The later delivery is an observer:
/// it must neither write its newer bytes under the old marker nor
/// quarantine the elected writer while that writer is in flight.
#[tokio::test]
#[serial(env)]
async fn changed_source_terminal_duplicate_observes_elected_writer_without_quarantine() {
    let (dir, conn) = ingest_fresh_conn().await;
    // Ingress canonicalizes the repository root before resolving the
    // provider source. On macOS `/var` and `/private/var` otherwise name
    // distinct fixture slugs and accidentally exercise a no-source
    // fallback instead of the source-fence path.
    let repo_path = dir
        .path()
        .canonicalize()
        .expect("canonicalize source-fence test repository");
    std::fs::create_dir_all(repo_path.join("objects"))
        .expect("create source-fence fixture object storage");
    let home = tempfile::tempdir().expect("test Claude home");
    let _home_guard = TestHomeGuard::set(home.path());

    let result = async {
        let session = "S-terminal-source-adoption";
        let transcript_dir = crate::internal::ai::observed_agents::claude_session_dir(&repo_path)
            .expect("test home resolves Claude source root");
        std::fs::create_dir_all(&transcript_dir).expect("create Claude transcript root");
        let transcript_path = transcript_dir.join(format!("{session}.jsonl"));
        // These are deliberately non-semantic provider metadata lines:
        // they produce distinct authorized source digests without
        // reserving coverage turns, so the test reaches the terminal
        // source-fence election rather than a coverage lease shortcut.
        let source_x = concat!(r#"{"type":"summary","marker":"source-x"}"#, "\n");
        std::fs::write(&transcript_path, source_x).expect("write elected source fixture");

        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", session, json!({})),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start source-adoption fixture");

        let terminal = ingest_envelope(
            "SessionEnd",
            session,
            json!({"event_id": "terminal-source-adoption-v1"}),
        );
        let pause =
            crate::internal::ai::capture::checkpoint::test_support::pause_after_registration_once(
                &repo_path,
            );
        let elected_conn = conn.clone();
        let elected_repo = repo_path.clone();
        let elected_terminal = terminal.clone();
        let elected = tokio::spawn(async move {
            ingest_agent_traces_payload(
                &elected_terminal,
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
                claude_provider(),
                &elected_conn,
                Some(&elected_repo),
            )
            .await
        });
        pause.wait_until_entered().await;

        let before = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, metadata_json FROM agent_session WHERE provider_session_id = ?",
                [session.into()],
            ))
            .await
            .expect("read elected pending receipt")
            .expect("elected terminal row exists");
        let before_state: String = before.try_get_by("state").expect("decode pending state");
        let before_metadata: String = before
            .try_get_by("metadata_json")
            .expect("decode pending receipt metadata");
        let before_metadata_value: serde_json::Value =
            serde_json::from_str(&before_metadata).expect("pending receipt metadata JSON");
        let source_x_commitment = durable_terminal_source_commitment(&before_metadata_value);

        let source_y = concat!(r#"{"type":"summary","marker":"source-y"}"#, "\n");
        std::fs::write(&transcript_path, source_y).expect("write later source fixture");
        assert_ne!(
            source_x, source_y,
            "the sibling must exercise a genuinely changed authorized source"
        );
        let later = ingest_agent_traces_payload(
            &terminal,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect_err("changed source must not write under the elected marker");
        assert!(
            format!("{later:#}").contains("earlier source snapshot"),
            "changed source must be a retry-only observer, got: {later:#}"
        );

        let after_observer = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, metadata_json,
                        (SELECT COUNT(*) FROM agent_checkpoint c
                         WHERE c.session_id = agent_session.session_id) AS checkpoints
                     FROM agent_session WHERE provider_session_id = ?",
                [session.into()],
            ))
            .await
            .expect("read changed-source observer state")
            .expect("observer must retain elected row");
        assert_eq!(
            after_observer
                .try_get_by::<String, _>("state")
                .expect("decode observer state"),
            before_state,
            "changed-source observer cannot publish terminal state"
        );
        assert_eq!(
            after_observer
                .try_get_by::<String, _>("metadata_json")
                .expect("decode observer metadata"),
            before_metadata,
            "changed-source observer cannot mutate or quarantine the elected finalizer"
        );
        assert_eq!(
            after_observer
                .try_get_by::<i64, _>("checkpoints")
                .expect("decode observer checkpoint count"),
            0,
            "changed-source observer must not append under the elected marker"
        );

        pause.release();
        elected
            .await
            .expect("join elected source writer")
            .expect("elected source finishes the terminal checkpoint");

        let completed = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, stopped_at, metadata_json,
                        (SELECT COUNT(*) FROM agent_checkpoint c
                         WHERE c.session_id = agent_session.session_id) AS checkpoints
                     FROM agent_session WHERE provider_session_id = ?",
                [session.into()],
            ))
            .await
            .expect("read elected completion")
            .expect("completed terminal row exists");
        assert_eq!(
            completed
                .try_get_by::<String, _>("state")
                .expect("decode completed state"),
            "stopped"
        );
        assert!(
            completed
                .try_get_by::<Option<i64>, _>("stopped_at")
                .expect("decode completed stopped_at")
                .is_some()
        );
        assert_eq!(
            completed
                .try_get_by::<i64, _>("checkpoints")
                .expect("decode completed checkpoint count"),
            1,
            "only the elected source may append the terminal checkpoint"
        );
        let completed_metadata: serde_json::Value = serde_json::from_str(
            &completed
                .try_get_by::<String, _>("metadata_json")
                .expect("decode completed receipt metadata"),
        )
        .expect("completed receipt metadata JSON");
        assert_eq!(
            completed_metadata["capture_catalog_receipts_v1"]["entries"][0]["status"],
            json!("complete")
        );
        assert_ne!(
            completed_metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["status"],
            json!("quarantined"),
            "the changed-source observer cannot poison the elected terminal writer"
        );
        assert_eq!(
            durable_terminal_source_commitment(&completed_metadata),
            source_x_commitment,
            "the changed-source observer cannot replace the elected source commitment"
        );

        Ok::<(), anyhow::Error>(())
    }
    .await;

    result.expect("changed-source terminal adoption regression");
}

/// If the elected source disappears before it has registered a marker,
/// a changed replay cannot safely publish its newer bytes under the old
/// source fence. The catalog therefore records a repair-required source
/// conflict, and the paused first writer is fenced inside the later
/// marker-registration transaction before it can append stale bytes.
#[tokio::test]
#[serial(env)]
async fn changed_source_before_terminal_marker_registration_quarantines_and_fences_late_writer() {
    let (dir, conn) = ingest_fresh_conn().await;
    // Match ingress's canonical repository spelling; see the companion
    // post-registration source-fence race test.
    let repo_path = dir
        .path()
        .canonicalize()
        .expect("canonicalize source-fence test repository");
    std::fs::create_dir_all(repo_path.join("objects"))
        .expect("create source-fence fixture object storage");
    let home = tempfile::tempdir().expect("test Claude home");
    let _home_guard = TestHomeGuard::set(home.path());

    let result = async {
        let session = "S-terminal-source-pre-registration";
        let transcript_dir = crate::internal::ai::observed_agents::claude_session_dir(&repo_path)
            .expect("test home resolves Claude source root");
        std::fs::create_dir_all(&transcript_dir).expect("create Claude transcript root");
        let transcript_path = transcript_dir.join(format!("{session}.jsonl"));
        let source_x = concat!(r#"{"type":"summary","marker":"source-x"}"#, "\n");
        std::fs::write(&transcript_path, source_x).expect("write elected source fixture");

        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", session, json!({})),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start source-conflict fixture");

        let terminal = ingest_envelope(
            "SessionEnd",
            session,
            json!({"event_id": "terminal-source-pre-registration-v1"}),
        );
        let pause =
            crate::internal::ai::capture::checkpoint::test_support::pause_before_registration_once(
                &repo_path,
            );
        let elected_conn = conn.clone();
        let elected_repo = repo_path.clone();
        let elected_terminal = terminal.clone();
        let elected = tokio::spawn(async move {
            ingest_agent_traces_payload(
                &elected_terminal,
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
                claude_provider(),
                &elected_conn,
                Some(&elected_repo),
            )
            .await
        });
        pause.wait_until_entered().await;

        let pending = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, metadata_json FROM agent_session WHERE provider_session_id = ?",
                [session.into()],
            ))
            .await
            .expect("read elected pre-registration receipt")
            .expect("elected terminal row exists");
        assert_eq!(
            pending
                .try_get_by::<String, _>("state")
                .expect("decode pending state"),
            "active",
            "catalog election remains pending before marker registration"
        );
        let pending_metadata: serde_json::Value = serde_json::from_str(
            &pending
                .try_get_by::<String, _>("metadata_json")
                .expect("decode elected pre-registration metadata"),
        )
        .expect("elected pre-registration receipt metadata JSON");
        let source_x_commitment = durable_terminal_source_commitment(&pending_metadata);

        let source_y = concat!(r#"{"type":"summary","marker":"source-y"}"#, "\n");
        std::fs::write(&transcript_path, source_y)
            .expect("write incompatible later source fixture");
        assert_ne!(
            source_x, source_y,
            "the replay must exercise a genuinely changed authorized source"
        );
        ingest_agent_traces_payload(
            &terminal,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("unregistered changed source leaves durable repair evidence");

        let quarantined = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, stopped_at, metadata_json,
                        (SELECT COUNT(*) FROM agent_checkpoint c
                         WHERE c.session_id = agent_session.session_id) AS checkpoints,
                        (SELECT COUNT(*) FROM metadata_kv m
                         WHERE m.scope = 'agent_traces_inflight'
                           AND m.target = agent_session.session_id) AS markers,
                        (SELECT COUNT(*) FROM reference r
                         WHERE r.name = ? AND r.kind = 'Branch'
                           AND r.remote IS NULL AND r.\"commit\" IS NOT NULL) AS trace_heads
                     FROM agent_session WHERE provider_session_id = ?",
                [
                    crate::internal::branch::TRACES_BRANCH.into(),
                    session.into(),
                ],
            ))
            .await
            .expect("read source-conflict quarantine")
            .expect("quarantined session exists");
        assert_eq!(
            quarantined
                .try_get_by::<String, _>("state")
                .expect("decode quarantined state"),
            "quarantined"
        );
        assert!(
            quarantined
                .try_get_by::<Option<i64>, _>("stopped_at")
                .expect("decode quarantined stopped_at")
                .is_none(),
            "source conflict must not publish terminal success"
        );
        assert_eq!(
            quarantined
                .try_get_by::<i64, _>("checkpoints")
                .expect("decode source-conflict checkpoints"),
            0,
            "neither source may publish a mixed checkpoint"
        );
        assert_eq!(
            quarantined
                .try_get_by::<i64, _>("markers")
                .expect("decode source-conflict markers"),
            0,
            "the incompatible replay must not manufacture a marker"
        );
        assert_eq!(
            quarantined
                .try_get_by::<i64, _>("trace_heads")
                .expect("decode source-conflict traces head"),
            0,
            "the incompatible replay must not advance refs/libra/traces"
        );
        let metadata: serde_json::Value = serde_json::from_str(
            &quarantined
                .try_get_by::<String, _>("metadata_json")
                .expect("decode source-conflict metadata"),
        )
        .expect("source-conflict metadata JSON");
        assert_eq!(
            metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["status"],
            json!("quarantined"),
            "the unavailable elected source is explicitly repair-required"
        );
        assert_eq!(
            metadata["capture_catalog_receipts_v1"]["entries"][0]["finalizer"]["quarantine_reason"],
            json!("source_digest_conflict")
        );
        assert_eq!(
            durable_terminal_source_commitment(&metadata),
            source_x_commitment,
            "the changed source must not replace the elected source commitment"
        );

        pause.release();
        let late = elected
            .await
            .expect("join fenced elected writer")
            .expect_err("late elected writer must revalidate its terminal receipt");
        assert!(
            format!("{late:#}").contains("catalog fence"),
            "late writer must be fenced after the source-conflict transition, got: {late:#}"
        );

        let after_late = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state,
                        (SELECT COUNT(*) FROM agent_checkpoint c
                         WHERE c.session_id = agent_session.session_id) AS checkpoints,
                        (SELECT COUNT(*) FROM metadata_kv m
                         WHERE m.scope = 'agent_traces_inflight'
                           AND m.target = agent_session.session_id) AS markers,
                        (SELECT COUNT(*) FROM reference r
                         WHERE r.name = ? AND r.kind = 'Branch'
                           AND r.remote IS NULL AND r.\"commit\" IS NOT NULL) AS trace_heads
                     FROM agent_session WHERE provider_session_id = ?",
                [
                    crate::internal::branch::TRACES_BRANCH.into(),
                    session.into(),
                ],
            ))
            .await
            .expect("read late-writer outcome")
            .expect("late-writer session exists");
        assert_eq!(
            after_late
                .try_get_by::<String, _>("state")
                .expect("decode late-writer state"),
            "quarantined"
        );
        assert_eq!(
            after_late
                .try_get_by::<i64, _>("checkpoints")
                .expect("decode late-writer checkpoints"),
            0
        );
        assert_eq!(
            after_late
                .try_get_by::<i64, _>("markers")
                .expect("decode late-writer markers"),
            0
        );
        assert_eq!(
            after_late
                .try_get_by::<i64, _>("trace_heads")
                .expect("decode late-writer traces head"),
            0
        );

        Ok::<(), anyhow::Error>(())
    }
    .await;

    result.expect("changed-source pre-registration recovery regression");
}

/// The post-registration fault is exercised in-process so a command
/// environment variable can never alter the shipped hook. The marker is
/// retired with its coverage claim before returning the typed failure;
/// the identical native terminal delivery then uses the elected receipt
/// marker to publish exactly one checkpoint and terminal state.
#[tokio::test]
async fn terminal_retry_after_post_registration_fault_reuses_its_elected_attempt() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-post-registration-fault";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start succeeds");

    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "post-registration-fault-v1"}),
    );
    crate::internal::ai::capture::checkpoint::test_support::fail_post_registration_once();
    assert!(
        ingest_agent_traces_payload(
            &end,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .is_err(),
        "the typed post-registration failure must interrupt before a checkpoint is durable"
    );

    let backend = conn.get_database_backend();
    let before = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count pre-replay checkpoints")
        .expect("checkpoint count row");
    assert_eq!(
        before.try_get_by::<i64, _>("n").unwrap(),
        0,
        "post-registration failure may not publish a partial checkpoint"
    );

    ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("same terminal replay succeeds after the registered marker is retired");
    let terminal = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query recovered terminal state")
        .expect("recovered session exists");
    assert_eq!(
        terminal.try_get_by::<String, _>("state").unwrap(),
        "stopped"
    );
    assert!(
        terminal
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_some()
    );
    let after = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count recovered checkpoints")
        .expect("checkpoint count row");
    assert_eq!(after.try_get_by::<i64, _>("n").unwrap(), 1);
}

/// Cleanup failure happens after the ref/catalog transaction. It must
/// leave durable recovery evidence rather than terminal publication; the
/// matching native replay clears the ordinary marker and completes from
/// the already-written checkpoint without appending another one.
#[tokio::test]
async fn terminal_retry_after_marker_cleanup_fault_completes_one_durable_checkpoint() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();
    let session = "S-marker-cleanup-fault";
    let start = ingest_envelope("SessionStart", session, json!({}));
    ingest_agent_traces_payload(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start succeeds");

    let end = ingest_envelope(
        "SessionEnd",
        session,
        json!({"event_id": "marker-cleanup-fault-v1"}),
    );
    crate::internal::ai::capture::checkpoint::test_support::fail_marker_cleanup_once();
    assert!(
        ingest_agent_traces_payload(
            &end,
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .is_err(),
        "cleanup failure must leave the terminal receipt pending"
    );

    let backend = conn.get_database_backend();
    let pending = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query pending terminal state")
        .expect("pending session exists");
    assert_ne!(pending.try_get_by::<String, _>("state").unwrap(), "stopped");
    assert!(
        pending
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_none()
    );

    ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("matching terminal replay clears ordinary marker cleanup");
    let terminal = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("query completed terminal state")
        .expect("completed session exists");
    assert_eq!(
        terminal.try_get_by::<String, _>("state").unwrap(),
        "stopped"
    );
    let checkpoints = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = \
                 (SELECT session_id FROM agent_session WHERE provider_session_id = ? LIMIT 1)",
            [session.into()],
        ))
        .await
        .expect("count replay checkpoints")
        .expect("checkpoint count row");
    assert_eq!(
        checkpoints.try_get_by::<i64, _>("n").unwrap(),
        1,
        "cleanup replay must not append a second checkpoint"
    );
}

/// ACF-05 AC4/AC7 step-by-step fault injection through the live terminal
/// path. Each checkpoint-store fault point fails the first delivery once
/// with its typed, content-free classification; deterministic recovery
/// (doctor repair where the store left ownership evidence) then lets the
/// identical native event complete exactly one checkpoint and one
/// receipt, and a further redelivery performs no second action.
#[tokio::test]
async fn terminal_replay_after_each_checkpoint_store_fault_completes_once() {
    use crate::internal::ai::capture::checkpoint::test_support::{
        self as checkpoint_faults, CheckpointFaultPoint as Point,
    };

    async fn session_row(
        conn: &DatabaseConnection,
        session: &str,
    ) -> (String, String, i64, Option<String>, serde_json::Value) {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT s.session_id, s.state, s.metadata_json,
                        (SELECT COUNT(*) FROM agent_checkpoint c
                         WHERE c.session_id = s.session_id) AS checkpoints,
                        (SELECT r.\"commit\" FROM reference r
                         WHERE r.name = ? AND r.kind = 'Branch'
                           AND r.remote IS NULL) AS traces_head
                     FROM agent_session s WHERE s.provider_session_id = ?",
                [
                    crate::internal::branch::TRACES_BRANCH.into(),
                    session.into(),
                ],
            ))
            .await
            .expect("query fault-matrix session")
            .expect("fault-matrix session exists");
        let metadata = serde_json::from_str(
            &row.try_get_by::<String, _>("metadata_json")
                .expect("decode fault-matrix receipts"),
        )
        .expect("fault-matrix receipt metadata JSON");
        (
            row.try_get_by("session_id")
                .expect("decode fault-matrix session id"),
            row.try_get_by("state").expect("decode fault-matrix state"),
            row.try_get_by("checkpoints")
                .expect("decode fault-matrix checkpoint count"),
            row.try_get_by("traces_head")
                .expect("decode fault-matrix traces head"),
            metadata,
        )
    }

    async fn checkpoint_commit(conn: &DatabaseConnection, session_id: &str) -> String {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT traces_commit FROM agent_checkpoint WHERE session_id = ?",
            [session_id.into()],
        ))
        .await
        .expect("query fault-matrix checkpoint")
        .expect("fault-matrix checkpoint exists")
        .try_get_by("traces_commit")
        .expect("decode fault-matrix checkpoint commit")
    }

    async fn inflight_markers(
        conn: &DatabaseConnection,
    ) -> Vec<crate::internal::ai::traces::TracesInflightMarker> {
        conn.query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT target, `key`, value FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'"
                .to_string(),
        ))
        .await
        .expect("list fault-matrix markers")
        .into_iter()
        .map(|row| {
            let target: String = row.try_get_by("target").expect("decode marker target");
            let key: String = row.try_get_by("key").expect("decode marker key");
            let value: String = row.try_get_by("value").expect("decode marker value");
            crate::internal::ai::traces::decode_and_validate_traces_inflight_marker(
                &value, &target, &key,
            )
            .expect("decode fault-matrix marker")
        })
        .collect()
    }

    fn assert_one_complete_receipt(point: Point, metadata: &serde_json::Value) {
        let entries = metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .unwrap_or_else(|| panic!("{point:?}: receipt ledger is missing"));
        assert_eq!(
            entries.len(),
            1,
            "{point:?}: one terminal receipt must remain"
        );
        assert_eq!(
            entries[0]["status"],
            json!("complete"),
            "{point:?}: the terminal receipt must complete exactly once"
        );
    }

    for point in Point::ALL {
        let (dir, conn) = ingest_fresh_conn().await;
        let repo_path = dir.path().to_path_buf();
        let session = format!("S-store-fault-{point:?}");
        let start = ingest_envelope("SessionStart", &session, json!({}));
        ingest_agent_traces_payload(
            &start,
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            Some(&repo_path),
        )
        .await
        .expect("start succeeds");
        let end = ingest_envelope(
            "SessionEnd",
            &session,
            json!({"event_id": format!("store-fault-{point:?}-v1")}),
        );
        let deliver = || {
            ingest_agent_traces_payload(
                &end,
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
                claude_provider(),
                &conn,
                Some(&repo_path),
            )
        };

        checkpoint_faults::fail_once_at(point);
        let failure = deliver()
            .await
            .expect_err("each store fault must interrupt the first delivery");
        assert_eq!(
            checkpoint_faults::armed_fault(),
            None,
            "{point:?}: the armed fault must fire on the first delivery"
        );
        let expected_class = match point {
            Point::MarkerRegistration | Point::AfterMarkerRegistration => "failed at marker",
            Point::AfterObjectWrite => "failed at object_write",
            Point::RefCasExhausted => "unchanged: RefCas",
            Point::StaleMarkerGeneration => "unchanged: StaleMarkerGeneration",
            Point::CompanionTransaction => "failed at catalog_transaction",
            Point::MarkerCleanup => "durable cleanup",
        };
        let rendered = format!("{failure:#}");
        assert!(
            rendered.contains(expected_class),
            "{point:?}: expected a typed `{expected_class}` failure, got: {rendered}"
        );
        assert!(
            !rendered.contains(&session),
            "{point:?}: the failure must not echo the provider session ID"
        );

        let (session_id, state, checkpoints, _, _) = session_row(&conn, &session).await;
        assert_ne!(
            state, "stopped",
            "{point:?}: the terminal receipt must not complete"
        );
        let committed_before_replay = point == Point::MarkerCleanup;
        assert_eq!(
            checkpoints,
            i64::from(committed_before_replay),
            "{point:?}: only a post-CAS cleanup fault has a durable checkpoint"
        );

        // Recovery evidence is part of the contract: objects created by a
        // rejected append stay owned by a cleanup-pending marker, a stale
        // writer leaves the takeover generation alone, and a post-CAS
        // cleanup fault keeps its refreshed ordinary marker.
        let markers = inflight_markers(&conn).await;
        let evidence_matches = match point {
            Point::MarkerRegistration | Point::AfterMarkerRegistration => markers.is_empty(),
            Point::AfterObjectWrite | Point::RefCasExhausted | Point::CompanionTransaction => {
                matches!(markers.as_slice(), [marker]
                    if marker.cleanup_pending && !marker.created_oids.is_empty())
            }
            Point::StaleMarkerGeneration => matches!(markers.as_slice(), [marker]
                if !marker.cleanup_pending && marker.started_at_ms == 0),
            Point::MarkerCleanup => matches!(markers.as_slice(), [marker]
                if !marker.cleanup_pending && marker.commit.is_some()),
        };
        assert!(
            evidence_matches,
            "{point:?}: unexpected recovery evidence: {markers:?}"
        );

        // Deterministic recovery: doctor retires ownership evidence that
        // an ordinary replay must not erase (cleanup-pending objects or
        // an expired takeover generation).
        for marker in markers {
            if marker.cleanup_pending || !marker.is_live(chrono::Utc::now().timestamp_millis()) {
                let storage =
                    std::sync::Arc::new(crate::utils::client_storage::ClientStorage::init(
                        // The repair runs over this repository's object store.
                        repo_path.join("objects"),
                    ));
                assert!(
                    crate::internal::ai::history::HistoryManager::for_traces(
                        storage,
                        repo_path.clone(),
                        std::sync::Arc::new(conn.clone()),
                    )
                    .repair_expired_traces_inflight_marker_for_test(
                        &marker.session_id,
                        &marker.attempt_id,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .await
                    .expect("doctor repair of the fault-matrix marker"),
                    "{point:?}: doctor repair must retire the recovery marker"
                );
            }
        }

        deliver()
            .await
            .unwrap_or_else(|error| panic!("{point:?}: recovered replay failed: {error:#}"));
        let (_, state, checkpoints, head, metadata) = session_row(&conn, &session).await;
        assert_eq!(state, "stopped", "{point:?}");
        assert_eq!(
            checkpoints, 1,
            "{point:?}: one durable checkpoint after replay"
        );
        assert_one_complete_receipt(point, &metadata);
        let commit = checkpoint_commit(&conn, &session_id).await;
        assert_eq!(
            head.as_deref(),
            Some(commit.as_str()),
            "{point:?}: refs/libra/traces must name the durable checkpoint"
        );
        assert!(
            inflight_markers(&conn).await.is_empty(),
            "{point:?}: a completed replay leaves no writer marker"
        );

        deliver()
            .await
            .unwrap_or_else(|error| panic!("{point:?}: duplicate replay failed: {error:#}"));
        let (_, state, checkpoints, duplicate_head, metadata) = session_row(&conn, &session).await;
        assert_eq!(state, "stopped", "{point:?}");
        assert_eq!(
            checkpoints, 1,
            "{point:?}: a duplicate replay must not append"
        );
        assert_one_complete_receipt(point, &metadata);
        assert_eq!(
            duplicate_head, head,
            "{point:?}: a duplicate replay must not advance refs/libra/traces"
        );
    }
}

/// Round-3 strengthened test: the redaction_report column should be
/// populated with at least one match when an envelope carries a known
/// secret, so the persisted row carries observable evidence the redactor
/// ran.
#[tokio::test]
async fn ingest_persists_observable_redaction_report() {
    let (_dir, conn) = ingest_fresh_conn().await;

    let payload = ingest_envelope(
        "UserPromptSubmit",
        "S-redact",
        json!({
            "prompt": "deploy with AKIAIOSFODNN7EXAMPLE please",
        }),
    );
    ingest_agent_traces_payload(
        &payload,
        ProviderHookCommand::Prompt,
        LifecycleEventKind::TurnStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("prompt ingest succeeds");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, redaction_report FROM agent_session WHERE provider_session_id = ?",
            ["S-redact".into()],
        ))
        .await
        .expect("query")
        .expect("row");

    assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "active");

    let report_json: String = row.try_get_by("redaction_report").unwrap();
    let report: serde_json::Value =
        serde_json::from_str(&report_json).expect("redaction_report is JSON");
    let matches = report
        .get("matches")
        .and_then(|v| v.as_array())
        .expect("matches is an array");
    assert!(
        !matches.is_empty(),
        "redaction_report.matches must be non-empty when prompt carries a known secret; got: {report_json}"
    );
    let bytes_redacted = report
        .get("bytes_redacted")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        bytes_redacted > 0,
        "redaction_report.bytes_redacted must be > 0; got {bytes_redacted}"
    );
    // The literal AKIA secret must NOT be reachable through the
    // persisted row — the only place we'd have stored its bytes is the
    // redaction_report, which now contains only positional matches.
    assert!(
        !report_json.contains("AKIAIOSFODNN7EXAMPLE"),
        "raw secret leaked into redaction_report column: {report_json}"
    );
}

/// Read the `metadata_json` for a provider session id as a JSON value.
async fn ingest_session_metadata(
    conn: &DatabaseConnection,
    provider_session_id: &str,
) -> serde_json::Value {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT metadata_json FROM agent_session WHERE provider_session_id = ?",
            [provider_session_id.into()],
        ))
        .await
        .expect("metadata query")
        .expect("session row");
    let json: String = row.try_get_by("metadata_json").unwrap();
    serde_json::from_str(&json).expect("metadata_json is valid JSON")
}

/// §6.3 state machine: a second concurrent session that submits a prompt
/// (`TurnStart`) while a peer is still `active` in the same `working_dir`
/// records `concurrent_active=true` in its session metadata, and a session
/// that never observed a peer at a turn stays unmarked.
#[tokio::test]
async fn ingest_turn_start_marks_concurrent_active_with_peer_in_same_workdir() {
    let (_dir, conn) = ingest_fresh_conn().await;

    // Session A starts and stays active (shared cwd `/tmp/repo`).
    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", "S-A", json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session A start ingest succeeds");

    // Session B starts in the same working_dir.
    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", "S-B", json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session B start ingest succeeds");

    // Session B submits a prompt → must observe A and flag concurrency.
    ingest_agent_traces_payload(
        &ingest_envelope("UserPromptSubmit", "S-B", json!({ "prompt": "hello" })),
        ProviderHookCommand::Prompt,
        LifecycleEventKind::TurnStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session B prompt ingest succeeds");

    let b_meta = ingest_session_metadata(&conn, "S-B").await;
    assert_eq!(
        b_meta.get("concurrent_active").and_then(|v| v.as_bool()),
        Some(true),
        "B's TurnStart with an active peer must record concurrent_active=true: {b_meta}"
    );

    // A never ran a turn alongside a peer, so it stays unmarked.
    let a_meta = ingest_session_metadata(&conn, "S-A").await;
    assert!(
        a_meta.get("concurrent_active").is_none(),
        "A had no concurrent turn and must not be flagged: {a_meta}"
    );
}

/// A lone session's `TurnStart` with no peer active in the same
/// `working_dir` must not raise `concurrent_active`.
#[tokio::test]
async fn ingest_turn_start_without_peer_does_not_mark_concurrent_active() {
    let (_dir, conn) = ingest_fresh_conn().await;

    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", "S-solo", json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("solo session start ingest succeeds");

    ingest_agent_traces_payload(
        &ingest_envelope("UserPromptSubmit", "S-solo", json!({ "prompt": "hi" })),
        ProviderHookCommand::Prompt,
        LifecycleEventKind::TurnStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("solo session prompt ingest succeeds");

    let meta = ingest_session_metadata(&conn, "S-solo").await;
    assert!(
        meta.get("concurrent_active").is_none(),
        "a lone session must not be flagged concurrent_active: {meta}"
    );
}

/// Once a turn marks a session `concurrent_active`, the marker is sticky:
/// later events merge (`json_patch`) into the existing metadata rather
/// than overwriting it, so a subsequent turn whose envelope no longer has
/// a peer (and so omits the flag) cannot clear the marker. Provider
/// transcript pointers are intentionally not catalog metadata.
#[tokio::test]
async fn ingest_metadata_merge_preserves_marker_without_transcript_locator() {
    let (_dir, conn) = ingest_fresh_conn().await;

    for sid in ["S-A", "S-B"] {
        ingest_agent_traces_payload(
            &ingest_envelope("SessionStart", sid, json!({})),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session start ingest succeeds");
    }

    // B's first turn observes A and is marked.
    ingest_agent_traces_payload(
        &ingest_envelope("UserPromptSubmit", "S-B", json!({ "prompt": "hello" })),
        ProviderHookCommand::Prompt,
        LifecycleEventKind::TurnStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session B first prompt ingest succeeds");

    // A stops, so no peer remains active.
    ingest_agent_traces_payload(
        &ingest_envelope("SessionEnd", "S-A", json!({})),
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session A end ingest succeeds");

    // B's second turn (peer gone) carries a different raw transcript
    // pointer and omits the flag. The marker remains, but the pointer is
    // not accepted as durable catalog state.
    let second_turn = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "S-B",
        "cwd": "/tmp/repo",
        "transcript_path": "/tmp/repo/transcript-2.jsonl",
        "prompt": "second turn",
    });
    ingest_agent_traces_payload(
        &serde_json::to_vec(&second_turn).expect("serialize envelope"),
        ProviderHookCommand::Prompt,
        LifecycleEventKind::TurnStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session B second prompt ingest succeeds");

    let meta = ingest_session_metadata(&conn, "S-B").await;
    assert_eq!(
        meta.get("concurrent_active").and_then(|v| v.as_bool()),
        Some(true),
        "marker must survive a later peer-free turn via metadata merge: {meta}"
    );
    assert!(
        meta.get("transcript_path").is_none(),
        "untrusted transcript locators must never be merged into metadata: {meta}"
    );
}

#[tokio::test]
async fn ingest_rejects_kind_mismatch() {
    let (_dir, conn) = ingest_fresh_conn().await;

    let payload = ingest_envelope("SessionStart", "S-mismatch", json!({}));
    let err = ingest_agent_traces_payload(
        &payload,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect_err("kind mismatch must fail");
    assert!(
        err.to_string().contains("hook event kind mismatch"),
        "unexpected error: {err}"
    );

    // No row should have been written.
    let backend = conn.get_database_backend();
    let count_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM agent_session WHERE provider_session_id = ?",
            ["S-mismatch".into()],
        ))
        .await
        .expect("count query")
        .expect("count row");
    assert_eq!(count_row.try_get_by::<i64, _>("n").unwrap(), 0);
}

#[tokio::test]
async fn ingest_fails_loud_when_table_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("noschema.db");
    std::fs::File::create(&path).expect("touch sqlite file");
    let url = format!("sqlite://{}", path.display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    let conn = Database::connect(opts).await.expect("connect");
    // intentionally NOT calling run_builtin_migrations.
    //
    // Ingest resolves the `CaptureScope` identity (`libra.repoid` in
    // `config_kv`) before it touches `agent_session`, so a truly bare
    // database would fail identity resolution first. Seed just that one
    // table — the way production `libra init` stamps it — so the loud
    // "agent_session table does not exist" failure under test stays the
    // first error.
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(
        backend,
        "CREATE TABLE config_kv (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                encrypted INTEGER NOT NULL DEFAULT 0
            )"
        .to_string(),
    ))
    .await
    .expect("create bare config_kv");
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        ["test-repo-id".into()],
    ))
    .await
    .expect("seed libra.repoid for identity resolution");

    let payload = ingest_envelope("SessionStart", "S-bare", json!({}));
    let err = ingest_agent_traces_payload(
        &payload,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect_err("missing table must fail");
    assert!(
        err.to_string()
            .contains("agent_session table does not exist"),
        "unexpected error: {err}"
    );
}

/// entire.md §6.3: a `TurnEnd` (Stop) event with a `repo_path` must also
/// materialise a `committed` checkpoint (per-turn rewind granularity)
/// while leaving the session `active` — checkpoints are no longer
/// SessionEnd-only.
#[tokio::test]
async fn ingest_turn_end_writes_committed_checkpoint() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();

    ingest_agent_traces_payload(
        &ingest_envelope("SessionStart", "S-turn-cp", json!({})),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start ok");

    // TurnEnd (Stop): the session stays active, but a committed checkpoint
    // must be written for the turn.
    ingest_agent_traces_payload(
        &ingest_envelope("Stop", "S-turn-cp", json!({})),
        ProviderHookCommand::Stop,
        LifecycleEventKind::TurnEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("turn end ok");

    let backend = conn.get_database_backend();
    let state_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state FROM agent_session WHERE provider_session_id = 'S-turn-cp'",
            [],
        ))
        .await
        .expect("state query")
        .expect("session row");
    assert_eq!(
        state_row.try_get_by::<String, _>("state").unwrap(),
        "active",
        "a TurnEnd must not stop the session"
    );

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT scope, traces_commit FROM agent_checkpoint \
                 WHERE session_id = (SELECT session_id FROM agent_session \
                   WHERE provider_session_id = 'S-turn-cp' LIMIT 1)",
            [],
        ))
        .await
        .expect("checkpoint query")
        .expect("a committed checkpoint must exist for the TurnEnd");
    assert_eq!(row.try_get_by::<String, _>("scope").unwrap(), "committed");
    assert!(
        !row.try_get_by::<String, _>("traces_commit")
            .unwrap()
            .is_empty(),
        "checkpoint must reference a non-empty traces commit"
    );
}

/// Phase 2.1: when a `repo_path` is supplied and SessionEnd fires, the
/// runtime must (a) write a checkpoint commit on `refs/libra/traces`
/// and (b) insert a row into `agent_checkpoint`. The checkpoint blob /
/// commit objects live under `<repo>/objects/`, so we point the test at a
/// fresh tempdir for that side too.
#[tokio::test]
async fn ingest_session_end_writes_checkpoint_when_repo_path_provided() {
    let (dir, conn) = ingest_fresh_conn().await;
    // Use the same tempdir as the SQLite file so the objects directory
    // and DB live together. We never need to run `libra init` here —
    // `append_checkpoint_commit` only needs the objects/ directory and
    // a sea-orm connection.
    let repo_path = dir.path().to_path_buf();

    let start = ingest_envelope("SessionStart", "S-cp", json!({}));
    ingest_agent_traces_payload(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start ok");

    // SessionEnd must trigger checkpoint creation.
    let end = ingest_envelope("SessionEnd", "S-cp", json!({}));
    ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("end ok");

    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, scope, traces_commit, tree_oid, metadata_blob_oid \
                 FROM agent_checkpoint WHERE session_id = (SELECT session_id FROM agent_session \
                  WHERE provider_session_id = 'S-cp' LIMIT 1)",
            [],
        ))
        .await
        .expect("query")
        .expect("checkpoint row exists");
    assert_eq!(row.try_get_by::<String, _>("scope").unwrap(), "committed");
    let traces_commit: String = row.try_get_by("traces_commit").unwrap();
    let tree_oid: String = row.try_get_by("tree_oid").unwrap();
    let metadata_blob_oid: String = row.try_get_by("metadata_blob_oid").unwrap();
    assert!(!traces_commit.is_empty());
    assert!(!tree_oid.is_empty());
    assert!(!metadata_blob_oid.is_empty());

    let completed_session = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at FROM agent_session WHERE provider_session_id = 'S-cp'",
            [],
        ))
        .await
        .expect("query completed session")
        .expect("completed session row exists");
    assert_eq!(
        completed_session.try_get_by::<String, _>("state").unwrap(),
        "stopped",
        "SessionEnd only publishes stopped after its checkpoint succeeds"
    );
    assert!(
        completed_session
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap()
            .is_some(),
        "completed terminal receipt publishes stopped_at"
    );

    // The metadata blob must exist on disk and parse as JSON whose
    // `agent_kind` matches what we ingested.
    let metadata_path = repo_path
        .join("objects")
        .join(&metadata_blob_oid[..2])
        .join(&metadata_blob_oid[2..]);
    assert!(
        metadata_path.exists(),
        "metadata blob missing at {metadata_path:?}"
    );

    // The traces ref row must point at the checkpoint commit hash.
    let backend = conn.get_database_backend();
    let ref_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT `commit` FROM reference WHERE name = ? AND kind = 'Branch' LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .expect("query traces ref")
        .expect("traces ref row exists");
    let head: String = ref_row.try_get_by("commit").unwrap();
    assert_eq!(head, traces_commit);

    // Phase 3.5c acceptance: every object touched by the agent
    // capture history must be tagged in `object_index` so cloud sync
    // uploads them. Without this, `libra cloud restore` would
    // resolve the orphan ref's commits to missing trees/blobs on a
    // fresh clone. The transcript blob carries the distinguished
    // `agent_transcript` o_type per entire.md §14.3.
    //
    // Codex round-1 follow-up: walk the *entire* reachability set
    // (commit → root tree → … → leaf blobs) rather than spot-
    // checking the root tree only. Walking the actual on-disk
    // objects catches new code paths that forget to call
    // `write_tree_indexed` for an intermediate tree.
    crate::utils::client_storage::ClientStorage::wait_for_background_tasks();

    verify_full_reachability_indexed(&conn, &repo_path, &traces_commit).await;

    // Spot check the metadata blob OID (Phase 3.5b's
    // `agent_checkpoint.metadata_blob_oid` column should join cleanly
    // to `object_index`).
    let metadata_count_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM object_index WHERE o_id = ?",
            [metadata_blob_oid.clone().into()],
        ))
        .await
        .expect("query metadata count")
        .expect("count row");
    let metadata_count: i64 = metadata_count_row.try_get_by("n").unwrap();
    assert_eq!(metadata_count, 1, "metadata blob is indexed");

    // Spot check the distinctive `agent_transcript` tag — at least
    // one row carries it (the transcript blob), per the spec.
    let transcript_count_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM object_index WHERE o_type = 'agent_transcript'",
            [],
        ))
        .await
        .expect("query agent_transcript count")
        .expect("count row");
    let transcript_count: i64 = transcript_count_row.try_get_by("n").unwrap();
    assert_eq!(
        transcript_count, 1,
        "exactly one transcript blob carries the agent_transcript o_type"
    );

    let _ = tree_oid; // silence unused warning — assertions above
    // already verified the root tree is indexed
    // via the reachability walker
}

/// entire.md §8.1 / §13 (P0) end-to-end: a SessionEnd prompt carrying
/// a known secret must land in the `traces` transcript blob
/// REDACTED — the raw secret never reaches durable storage. Guards the
/// `RedactedBytes` write-path contract: the transcript blob is produced
/// only via `RedactedBytes`, and the upstream redactor scrubbed the
/// secret before it was wrapped. A regression that bypassed redaction
/// (or the type) would surface here as the literal key in the blob.
#[tokio::test]
async fn session_end_checkpoint_transcript_blob_is_redacted() {
    let (dir, conn) = ingest_fresh_conn().await;
    let repo_path = dir.path().to_path_buf();

    let start = ingest_envelope("SessionStart", "S-cp-redact", json!({}));
    ingest_agent_traces_payload(
        &start,
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start ok");

    // SessionEnd whose prompt carries a known AWS-key-shaped secret.
    let end = ingest_envelope(
        "SessionEnd",
        "S-cp-redact",
        json!({ "prompt": "deploy with AKIAIOSFODNN7EXAMPLE please" }),
    );
    ingest_agent_traces_payload(
        &end,
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("end ok");

    crate::utils::client_storage::ClientStorage::wait_for_background_tasks();

    // Locate the transcript blob via its distinguished o_type, then
    // read + zlib-decode it and strip the `blob <len>\0` header.
    let backend = conn.get_database_backend();
    let blob_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT o_id FROM object_index WHERE o_type = 'agent_transcript' LIMIT 1",
            [],
        ))
        .await
        .expect("query transcript blob")
        .expect("a transcript blob must be indexed");
    let blob_oid: String = blob_row.try_get_by("o_id").unwrap();

    let object_path = repo_path
        .join("objects")
        .join(&blob_oid[..2])
        .join(&blob_oid[2..]);
    let raw = std::fs::read(&object_path).expect("read transcript blob object");
    let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
    let header_end = decoded
        .iter()
        .position(|&b| b == 0)
        .expect("blob object has a header terminator");
    let body = String::from_utf8_lossy(&decoded[header_end + 1..]);

    assert!(
        !body.contains("AKIAIOSFODNN7EXAMPLE"),
        "raw secret leaked into the persisted transcript blob: {body}",
    );
    assert!(
        body.contains("deploy with") && body.contains("please"),
        "the redacted transcript must retain the non-secret text, got: {body}",
    );
}

/// A6.5 regression: codex relocates its whole home via `$CODEX_HOME`
/// and every other part of the codex chain honors it
/// (`resolve_codex_home`), so the transcript trust gate must resolve
/// the same root — otherwise sessions under a relocated CODEX_HOME are
/// silently captured with empty transcripts (exactly what the A6.5
/// real-CLI smoke observed with its isolated CODEX_HOME).
#[test]
#[serial(env)]
fn codex_transcript_root_honors_codex_home_override() {
    let adapter = crate::internal::ai::observed_agents::agent_for(
        crate::internal::ai::observed_agents::AgentKind::Codex,
    );
    let codex_home = tempfile::tempdir().expect("codex home tempdir");
    let sessions = codex_home.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join("rollout-test.jsonl");
    std::fs::write(&rollout, "{}\n").unwrap();

    // Fake $HOME so the real ~/.codex can never accidentally match.
    let home = tempfile::tempdir().expect("fake home tempdir");
    let prior_home = std::env::var_os("LIBRA_TEST_HOME");
    let prior_codex = std::env::var_os("CODEX_HOME");
    // SAFETY: test-only env mutation, restored below; serialised via
    // #[serial] so it cannot race other env readers.
    unsafe {
        std::env::set_var("LIBRA_TEST_HOME", home.path());
        std::env::set_var("CODEX_HOME", codex_home.path());
    }
    let trusted = crate::internal::ai::observed_agents::transcript_path_within_provider_root(
        adapter, &rollout,
    );
    unsafe {
        std::env::remove_var("CODEX_HOME");
    }
    let untrusted = crate::internal::ai::observed_agents::transcript_path_within_provider_root(
        adapter, &rollout,
    );
    unsafe {
        match prior_codex {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match prior_home {
            Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
            None => std::env::remove_var("LIBRA_TEST_HOME"),
        }
    }
    assert!(
        trusted,
        "a rollout under $CODEX_HOME must pass the provider-root gate"
    );
    assert!(
        !untrusted,
        "without the override the relocated rollout stays untrusted"
    );
}

/// entire.md §6.3 / §7.1: the SessionEnd checkpoint transcript blob must
/// carry the agent's FULL on-disk transcript (read via the
/// `ObservedAgent::read_transcript` adapter), not just the closing
/// prompt, and that transcript must be redacted before storage. Writes a
/// real transcript file at the provider-derived `(cwd, session_id)`
/// location plus a secret, supplies a deliberately foreign hook pointer,
/// and asserts the persisted blob contains only the derived source.
#[tokio::test]
#[serial(env)]
async fn session_end_checkpoint_captures_full_transcript_via_adapter() {
    let (dir, conn) = ingest_fresh_conn().await;
    // The ingress fixture canonicalizes its verified repository binding.
    // Build the provider-derived Claude path from that same spelling so a
    // macOS `/tmp` → `/private/tmp` system alias cannot change the slug.
    let repo_path = dir
        .path()
        .canonicalize()
        .expect("canonicalize full-transcript test repository");
    // A real initialized repository has an object store. The durable
    // source commitment intentionally refuses to mint against the
    // database-only fixture, so model that storage before exercising the
    // complete checkpoint path.
    std::fs::create_dir_all(repo_path.join("objects"))
        .expect("create full-transcript fixture object storage");

    // The transcript must live under the provider's home-relative root
    // (`~/.claude`) to pass the security trust check, so stand up a fake
    // HOME via LIBRA_TEST_HOME and place the file there. It carries content
    // the closing prompt does NOT contain plus an AWS-key-shaped secret
    // that must be redacted.
    let home = tempfile::tempdir().expect("fake home tempdir");
    let test_home = TestHomeGuard::set(home.path());
    let session_id = "S-full-transcript";
    // The in-process ingress seam replaces the envelope's claimed cwd
    // with this verified repository binding, so derive the fixture source
    // from the same canonical root the live runtime will use.
    let claude_dir = crate::internal::ai::observed_agents::claude_session_dir(&repo_path)
        .expect("fake home provides a Claude project directory");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let transcript_path = claude_dir.join(format!("{session_id}.jsonl"));
    std::fs::write(
        &transcript_path,
        "user: kick off the deploy\nassistant: full-transcript-marker-9f3 with AKIAIOSFODNN7EXAMPLE\n",
    )
    .unwrap();
    // This is intentionally NOT the provider-derived file. It used to be
    // trusted through `envelope.transcript_path`; retaining it would let a
    // sibling repository smuggle a transcript into this checkpoint.
    let foreign_pointer_marker = "foreign-hook-pointer-marker-4a8";
    let foreign_path = home.path().join(".claude").join("foreign.jsonl");
    std::fs::write(&foreign_path, foreign_pointer_marker).unwrap();
    let foreign_path_str = foreign_path.to_string_lossy().to_string();

    // The library-unit executable cannot dispatch the production private
    // authorized-read helper. Keep this test on the real,
    // provider-derived local source through the narrowly scoped unit-test
    // boundary rather than silently falling back to the closing prompt.
    let _source_boundary = test_support::capture_live_without_deadline();

    let envelope = |hook: &str, prompt: Option<&str>| -> Vec<u8> {
        let mut base = json!({
            "hook_event_name": hook,
            "session_id": session_id,
            "cwd": "/tmp/repo",
            "transcript_path": foreign_path_str,
        });
        if let (Some(p), Some(obj)) = (prompt, base.as_object_mut()) {
            obj.insert("prompt".to_string(), json!(p));
        }
        serde_json::to_vec(&base).unwrap()
    };

    ingest_agent_traces_payload(
        &envelope("SessionStart", None),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("start ok");

    // Closing prompt deliberately omits the transcript marker so the test
    // can distinguish "captured the prompt" from "captured the transcript".
    ingest_agent_traces_payload(
        &envelope("SessionEnd", Some("wrap up now")),
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        Some(&repo_path),
    )
    .await
    .expect("end ok");

    // Restore the environment before the (env-independent) assertions;
    // `TestHomeGuard` also makes this unwind-safe.
    drop(test_home);

    crate::utils::client_storage::ClientStorage::wait_for_background_tasks();

    let backend = conn.get_database_backend();
    let blob_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT o_id FROM object_index WHERE o_type = 'agent_transcript' LIMIT 1",
            [],
        ))
        .await
        .expect("query transcript blob")
        .expect("a transcript blob must be indexed");
    let blob_oid: String = blob_row.try_get_by("o_id").unwrap();

    let object_path = repo_path
        .join("objects")
        .join(&blob_oid[..2])
        .join(&blob_oid[2..]);
    let raw = std::fs::read(&object_path).expect("read transcript blob object");
    let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
    let header_end = decoded
        .iter()
        .position(|&b| b == 0)
        .expect("blob object has a header terminator");
    let body = String::from_utf8_lossy(&decoded[header_end + 1..]);

    assert!(
        body.contains("full-transcript-marker-9f3"),
        "checkpoint must capture the full transcript via the adapter, not just the prompt: {body}",
    );
    assert!(
        !body.contains("AKIAIOSFODNN7EXAMPLE"),
        "the secret in the transcript must be redacted before storage: {body}",
    );
    assert!(
        !body.contains("wrap up now"),
        "the full transcript should replace the prompt-only stopgap: {body}",
    );
    assert!(
        !body.contains(foreign_pointer_marker),
        "a raw hook transcript pointer must not select checkpoint bytes: {body}",
    );
}

/// Walk every object reachable from the checkpoint commit (commit →
/// root tree → recursively trees/blobs) and assert each OID appears
/// in `object_index`. Used to guard against future regressions where
/// a write path forgets to route through `write_tree_indexed` or
/// the indexing helper.
async fn verify_full_reachability_indexed(
    conn: &DatabaseConnection,
    repo_path: &std::path::Path,
    commit_oid: &str,
) {
    let mut to_walk: Vec<(String, &'static str)> = vec![(commit_oid.to_string(), "commit")];
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

    while let Some((oid, expected_type)) = to_walk.pop() {
        if !visited.insert(oid.clone()) {
            continue;
        }
        assert_object_index_has(conn, &oid, expected_type).await;
        // Read the on-disk Git object to discover its references.
        let object_path = repo_path.join("objects").join(&oid[..2]).join(&oid[2..]);
        let raw = std::fs::read(&object_path).unwrap_or_else(|e| panic!("read object {oid}: {e}"));
        let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        let header_end = decoded.iter().position(|&b| b == 0).unwrap();
        let header = std::str::from_utf8(&decoded[..header_end]).unwrap();
        let body = &decoded[header_end + 1..];
        if header.starts_with("commit ") {
            let body_text = std::str::from_utf8(body).unwrap();
            let tree_line = body_text.lines().next().expect("commit has tree line");
            let tree_oid = tree_line.strip_prefix("tree ").expect("tree prefix");
            to_walk.push((tree_oid.to_string(), "tree"));
        } else if header.starts_with("tree ") {
            // Tree entry: `<mode> <name>\0<20 raw bytes>` (SHA-1).
            let mut cursor = 0;
            while cursor < body.len() {
                let space_pos = cursor
                    + body[cursor..]
                        .iter()
                        .position(|&b| b == b' ')
                        .expect("mode terminator");
                let mode = std::str::from_utf8(&body[cursor..space_pos]).unwrap();
                let name_start = space_pos + 1;
                let null_pos = name_start
                    + body[name_start..]
                        .iter()
                        .position(|&b| b == 0)
                        .expect("name terminator");
                let hash_start = null_pos + 1;
                let hash_bytes = &body[hash_start..hash_start + 20];
                let child_oid = hex::encode(hash_bytes);
                let child_type = if mode == "40000" { "tree" } else { "blob" };
                to_walk.push((child_oid, child_type));
                cursor = hash_start + 20;
            }
        }
    }
}

async fn assert_object_index_has(conn: &DatabaseConnection, oid: &str, expected_o_type: &str) {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT o_type FROM object_index WHERE o_id = ? LIMIT 1",
            [oid.into()],
        ))
        .await
        .unwrap_or_else(|e| panic!("query object_index for {oid}: {e}"))
        .unwrap_or_else(|| panic!("object {oid} missing from object_index"));
    let actual: String = row.try_get_by("o_type").unwrap();
    // A blob may be tagged with a more-specific agent o_type
    // (`agent_transcript`); accept that as a valid upgrade.
    let acceptable =
        expected_o_type == actual || (expected_o_type == "blob" && actual.starts_with("agent_"));
    assert!(
        acceptable,
        "object {oid} has o_type '{actual}', expected '{expected_o_type}' (or agent_* upgrade)"
    );
}

// -------------------------------------------------------------------
// ACF-20 VER1: the export-job runner token is settled on every exit
// -------------------------------------------------------------------

mod export_runner_lease {
    use std::{collections::BTreeSet, sync::Mutex};

    use super::*;
    use crate::internal::ai::capture::{
        catalog::CaptureCatalogConflict,
        checkpoint::{CheckpointConflictReason, CheckpointStoreError, CheckpointStoreStage},
        coordinator::CoordinatorReservationClass,
        live::{LeaseLogContext, LiveCaptureReservation, LiveExportAdmission},
        live_checkpoint::{CheckpointStageExit, checkpoint_stage_lease_disposition},
    };

    /// Every lease-port call a fake records, in order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum LeaseCall {
        ObserveIdle,
        ReleaseFailed,
        ReleaseDirty,
        AdvanceAndRelease,
    }

    /// Fake `LiveExportLeasePort`: records each call with its target, can
    /// refuse admission, fail the advance, or fail every release.
    #[derive(Default)]
    struct FakeExportLease {
        calls: Mutex<Vec<LeaseCall>>,
        targets: Mutex<Vec<(String, String)>>,
        recorded_only: bool,
        fail_advance: bool,
        fail_release: bool,
    }

    impl FakeExportLease {
        fn calls(&self) -> Vec<LeaseCall> {
            self.calls.lock().expect("fake lease call log").clone()
        }

        fn record(&self, call: LeaseCall, target: &LiveExportTarget<'_>) {
            self.calls.lock().expect("fake lease call log").push(call);
            self.targets.lock().expect("fake lease target log").push((
                target.agent_kind.to_string(),
                target.provider_session_id.to_string(),
            ));
        }

        fn release(&self) -> Result<()> {
            if self.fail_release {
                bail!("fake export lease release failed");
            }
            Ok(())
        }
    }

    impl LiveExportLeasePort for FakeExportLease {
        async fn observe_idle(
            &self,
            target: &LiveExportTarget<'_>,
            owner: &str,
            _now_ms: i64,
            _deadline: CaptureCommitDeadline,
        ) -> Result<LiveExportAdmission> {
            assert!(
                owner.starts_with("export:"),
                "runner owners keep the export:{{pid}}:{{uuid}} format: {owner}"
            );
            self.record(LeaseCall::ObserveIdle, target);
            Ok(if self.recorded_only {
                LiveExportAdmission::RecordedOnly
            } else {
                LiveExportAdmission::Runner {
                    fence_token: 7,
                    target_generation: 3,
                }
            })
        }

        async fn release_failed(
            &self,
            target: &LiveExportTarget<'_>,
            _owner: &str,
            fence_token: i64,
            _deadline: CaptureCommitDeadline,
        ) -> Result<()> {
            assert_eq!(fence_token, 7);
            self.record(LeaseCall::ReleaseFailed, target);
            self.release()
        }

        async fn release_dirty(
            &self,
            target: &LiveExportTarget<'_>,
            _owner: &str,
            fence_token: i64,
            _deadline: CaptureCommitDeadline,
        ) -> Result<()> {
            assert_eq!(fence_token, 7);
            self.record(LeaseCall::ReleaseDirty, target);
            self.release()
        }

        async fn advance_and_release(
            &self,
            target: &LiveExportTarget<'_>,
            _owner: &str,
            fence_token: i64,
            target_generation: i64,
            _deadline: CaptureCommitDeadline,
        ) -> Result<()> {
            assert_eq!((fence_token, target_generation), (7, 3));
            self.record(LeaseCall::AdvanceAndRelease, target);
            if self.fail_advance {
                bail!("fake export generation advance failed");
            }
            Ok(())
        }
    }

    /// A real catalog reservation for the one checkpoint-stage variant that
    /// owns one; it is dropped without any lease work.
    async fn sample_reservation(
        conn: &DatabaseConnection,
    ) -> (LiveCaptureReservation, CaptureCatalogApplyRequest) {
        let scope = CaptureScope::main_for_connection(conn)
            .await
            .expect("resolve sample reservation scope");
        let event_id = uuid::Uuid::new_v4();
        let plan = reduce_lifecycle(LifecycleReducerInput {
            current: None,
            event_kind: LifecycleEventKind::SessionStart,
            event_id,
            occurred_at: 1_700_000_000,
            deadline: None,
        })
        .expect("reduce sample lifecycle action");
        let request = CaptureCatalogApplyRequest::new(
            scope,
            CaptureCatalogSession::new(
                "opencode__sample-reservation",
                "opencode",
                "sample-reservation",
                "/tmp/sample-reservation",
            )
            .expect("sample catalog session"),
            CaptureCatalogAction::from_ingress(event_id, None, LifecycleEventKind::SessionStart)
                .expect("sample catalog action"),
            CaptureCatalogMutation::from_reducer(None, &plan, 1_700_000_000)
                .expect("sample catalog mutation"),
        )
        .expect("sample catalog request");
        let reservation = match reserve_capture_catalog_until(conn.clone(), &request, None)
            .await
            .expect("reserve sample catalog request")
        {
            CaptureCoordinatorReserveOutcome::Reserved(reservation) => *reservation,
            _ => panic!("a fresh sample request must reserve"),
        };
        (reservation, request)
    }

    /// Every `CaptureCoordinatorError` variant; the exhaustive name match
    /// below makes a new variant fail to compile until it is listed here.
    fn coordinator_error_variant(error: &CaptureCoordinatorError) -> &'static str {
        use CaptureCoordinatorError as E;
        match error {
            E::UnexpectedCheckpoint => "UnexpectedCheckpoint",
            E::MissingCheckpoint => "MissingCheckpoint",
            E::CheckpointScopeMismatch => "CheckpointScopeMismatch",
            E::CheckpointReceiptMismatch => "CheckpointReceiptMismatch",
            E::MissingTerminalFinalizer => "MissingTerminalFinalizer",
            E::FinalizerReceiptMismatch => "FinalizerReceiptMismatch",
            E::UnexpectedFinalizer => "UnexpectedFinalizer",
            E::MissingPreappliedReservation => "MissingPreappliedReservation",
            E::PreappliedRequestMismatch => "PreappliedRequestMismatch",
            E::AdoptedActionRequiresReservation => "AdoptedActionRequiresReservation",
            E::TerminalCompletionWithoutCheckpoint => "TerminalCompletionWithoutCheckpoint",
            E::CatalogApply(_) => "CatalogApply",
            E::CheckpointWrite(_) => "CheckpointWrite",
            E::CheckpointWriteFinalizer { .. } => "CheckpointWriteFinalizer",
            E::CheckpointConflictFinalizer { .. } => "CheckpointConflictFinalizer",
            E::DurableCheckpointCompletion { .. } => "DurableCheckpointCompletion",
            E::CheckpointReplayCompletion { .. } => "CheckpointReplayCompletion",
            E::CatalogComplete(_) => "CatalogComplete",
            E::CatalogFinalize(_) => "CatalogFinalize",
            E::FinalizerUnexpectedReady => "FinalizerUnexpectedReady",
            E::FinalizerNotDurable => "FinalizerNotDurable",
            E::UnexpectedDurableFinalizerOutcome => "UnexpectedDurableFinalizerOutcome",
            E::CheckpointMarkerMismatch => "CheckpointMarkerMismatch",
            E::CompletionConflict { .. } => "CompletionConflict",
            E::InterruptedAfterCheckpoint => "InterruptedAfterCheckpoint",
        }
    }

    fn every_coordinator_error() -> Vec<(CaptureCoordinatorError, CoordinatorReservationClass)> {
        use CaptureCoordinatorError as E;
        use CoordinatorReservationClass as C;
        let store = || CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::ObjectWrite,
        };
        let nested = || Box::new(E::CatalogFinalize(CaptureCatalogError::InvalidRequest));
        vec![
            (E::UnexpectedCheckpoint, C::Unclassified),
            (E::MissingCheckpoint, C::Unclassified),
            (E::CheckpointScopeMismatch, C::Unclassified),
            (E::CheckpointReceiptMismatch, C::Unclassified),
            (E::MissingTerminalFinalizer, C::Unclassified),
            (E::FinalizerReceiptMismatch, C::Unclassified),
            (E::UnexpectedFinalizer, C::Unclassified),
            (E::MissingPreappliedReservation, C::Unclassified),
            (E::PreappliedRequestMismatch, C::Unclassified),
            (E::AdoptedActionRequiresReservation, C::Unclassified),
            (E::TerminalCompletionWithoutCheckpoint, C::Unclassified),
            (
                E::CatalogApply(CaptureCatalogError::InvalidRequest),
                C::Unclassified,
            ),
            (
                E::CheckpointWrite(store()),
                C::Uncommitted { diagnostic: true },
            ),
            (
                E::CheckpointWriteFinalizer {
                    checkpoint: store(),
                    finalizer: nested(),
                },
                C::Uncommitted { diagnostic: true },
            ),
            (
                E::CheckpointConflictFinalizer {
                    reason: CheckpointConflictReason::ScopeFence,
                    finalizer: nested(),
                },
                C::Uncommitted { diagnostic: false },
            ),
            (
                E::DurableCheckpointCompletion {
                    checkpoint_id: "checkpoint".to_string(),
                    cause: nested(),
                },
                C::PostCheckpoint,
            ),
            (
                E::CheckpointReplayCompletion {
                    checkpoint_id: "checkpoint".to_string(),
                    cause: nested(),
                },
                C::Uncommitted { diagnostic: false },
            ),
            (
                E::CatalogComplete(CaptureCatalogError::InvalidRequest),
                C::Unclassified,
            ),
            (
                E::CatalogFinalize(CaptureCatalogError::InvalidRequest),
                C::Unclassified,
            ),
            (E::FinalizerUnexpectedReady, C::Unclassified),
            (E::FinalizerNotDurable, C::Unclassified),
            (E::UnexpectedDurableFinalizerOutcome, C::Unclassified),
            (E::CheckpointMarkerMismatch, C::Unclassified),
            (
                E::CompletionConflict {
                    conflict: CaptureCatalogConflict::SessionIdentity,
                },
                C::Unclassified,
            ),
            (E::InterruptedAfterCheckpoint, C::Unclassified),
        ]
    }

    /// ADR-ACF-10 entry recount (ACF-20): every runner-admitted exit of the
    /// export path in `capture/live_checkpoint.rs`, its ADR class, the
    /// variant built at the exit and the lease disposition that class
    /// requires. `run_export_stage` exits build their `ExportStageExit`
    /// inline; checkpoint exits use the class helper named here.
    #[rustfmt::skip]
    const ENTRY_RECOUNT: &[(&str, &str, &str, &str, LeaseDisposition)] = &[
        // Source stage: claims not yet reserved.
        ("exporter bridge Err", "source", "BridgeUnavailable", "inline", LeaseDisposition::Failed),
        ("TranscriptSource::File", "source", "SourceRejected", "inline", LeaseDisposition::Failed),
        ("snapshot.transcript() None", "source", "SourceRejected", "inline", LeaseDisposition::Failed),
        ("source commitment unbound", "source", "SourceRejected", "inline", LeaseDisposition::Failed),
        ("snapshot bytes missing", "source", "SourceRejected", "inline", LeaseDisposition::Failed),
        // Post-coverage source: claims left to expire.
        ("into_redacted_transcript() None", "post-coverage source", "SourceRejected", "inline", LeaseDisposition::Failed),
        // Coverage stage.
        ("coverage reservation Err", "coverage", "CoverageFailed", "inline", LeaseDisposition::Dirty),
        ("in-flight-only skip", "coverage", "InflightOnly", "inline", LeaseDisposition::Dirty),
        ("covered replay Err/PendingCleanup/mismatch", "coverage", "CoveredReplayFailed", "inline", LeaseDisposition::Dirty),
        ("covered replay DeadlineElapsed", "coverage", "CoveredReplayFailed", "inline", LeaseDisposition::Dirty),
        ("nonterminal no-op", "coverage", "CoveredNoop", "inline", LeaseDisposition::AdvanceAndRelease),
        ("covered terminal replay", "coverage", "CoveredNoop", "inline", LeaseDisposition::AdvanceAndRelease),
        // Checkpoint stage (a): uncommitted.
        ("capture deadline before extraction", "(a)", "UncommittedSettle", "uncommitted_settle", LeaseDisposition::Dirty),
        ("capture deadline after extraction", "(a)", "UncommittedSettle", "uncommitted_settle", LeaseDisposition::Dirty),
        ("snapshot projection serialize", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("metadata serialize", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("canonical event serialize", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("redaction report serialize", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("HEAD resolve", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("redacted payload", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("finalizer policy", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("claim_terminal_checkpoint_attempt Err", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("DurableReplay completion Err/outcomes", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("terminal attempt Adopted", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("terminal attempt AlreadyComplete", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("terminal attempt Quarantined", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("terminal attempt ConflictUnchanged", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("retained artifact recheck Ok(true)/Err", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("checkpoint store", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("CheckpointWriteRequest", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("CaptureCoordinatorRequest", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("coordinator Err Uncommitted{diagnostic}", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("CheckpointAlreadyExists/AlreadyApplied", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("CheckpointInFlight", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("CheckpointConflict", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("FinalizerQuarantined", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("FinalizerPending", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("ConflictUnchanged", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        ("StateApplied", "(a)", "Uncommitted", "uncommitted", LeaseDisposition::Dirty),
        // Checkpoint stage (b): post-CAS, claims kept.
        ("coordinator Err PostCheckpoint", "(b)", "PostCheckpoint", "post_checkpoint", LeaseDisposition::Dirty),
        ("PendingCleanup", "(b)", "PostCheckpoint", "post_checkpoint", LeaseDisposition::Dirty),
        ("DurableFinalizer cleanup_pending", "(b)", "PostCheckpoint", "post_checkpoint", LeaseDisposition::Dirty),
        ("DurableFinalizer::Conflict", "(b)", "PostCheckpoint", "post_checkpoint", LeaseDisposition::Dirty),
        // Checkpoint stage (c): authorized.
        ("CheckpointCommitted", "(c)", "Authorized", "authorized", LeaseDisposition::AdvanceAndRelease),
        ("DurableFinalizer applied/quarantined, no cleanup", "(c)", "Authorized", "authorized", LeaseDisposition::AdvanceAndRelease),
        // Checkpoint stage (d): leave to expiry.
        ("coordinator Err Unclassified", "(d)", "Expire", "expire", LeaseDisposition::LeaveToExpiry),
        ("completed-runner capture_deadline invariant", "(d)", "Expire", "authorized→expire", LeaseDisposition::LeaveToExpiry),
    ];

    /// ACF-20 VER1, mapping half: `reservation_class` classifies every
    /// coordinator error variant, and both exhaustive stage mappings return
    /// the ADR-ACF-10 `(LeaseDisposition, reason, LeaseLogContext)` triple —
    /// reasons byte-identical to the pre-ACF-20 release sites — for every
    /// variant. The executable entry recount table agrees with them.
    #[tokio::test]
    async fn export_stage_releases_runner_lease_on_every_exit_mapping() {
        let classified = every_coordinator_error();
        let names: BTreeSet<&str> = classified
            .iter()
            .map(|(error, _)| coordinator_error_variant(error))
            .collect();
        assert_eq!(
            names.len(),
            25,
            "every coordinator error variant is listed once"
        );
        for (error, expected) in &classified {
            assert_eq!(
                error.reservation_class(),
                *expected,
                "{}",
                coordinator_error_variant(error)
            );
        }
        assert_eq!(
            classified
                .iter()
                .filter(|(_, class)| *class == CoordinatorReservationClass::Unclassified)
                .count(),
            20,
            "ADR-ACF-10: the remaining 20 variants are unclassified"
        );

        let session = "opencode__mapping-session";
        let session_log = |message| LeaseLogContext::Session {
            session_id: session,
            message,
        };
        let export_exits = [
            (
                ExportStageExit::BridgeUnavailable,
                (
                    LeaseDisposition::Failed,
                    "release_failed_export_runner_failed",
                    session_log("failed to release unsuccessful export job"),
                ),
            ),
            (
                ExportStageExit::SourceRejected(anyhow!("rejected")),
                (
                    LeaseDisposition::Failed,
                    "release_failed_export_runner_failed",
                    session_log("failed to release unsuccessful export job"),
                ),
            ),
            (
                ExportStageExit::CoverageFailed(anyhow!("coverage")),
                (
                    LeaseDisposition::Dirty,
                    "release_export_job_after_coverage_error_failed",
                    session_log("failed to release export job after coverage error"),
                ),
            ),
            (
                ExportStageExit::InflightOnly,
                (
                    LeaseDisposition::Dirty,
                    "release_export_job_after_inflight_skip_failed",
                    session_log("failed to release export job after in-flight skip"),
                ),
            ),
            (
                ExportStageExit::CoveredReplayFailed(anyhow!("replay")),
                (
                    LeaseDisposition::Dirty,
                    "release_export_job_after_covered_replay_failed",
                    session_log("failed to release export job after covered replay settlement"),
                ),
            ),
            (
                ExportStageExit::CoveredNoop {
                    covered_terminal_replay: true,
                },
                (
                    LeaseDisposition::AdvanceAndRelease,
                    "settle_noop_export_job_recovery_failed",
                    session_log("failed to retire timed-out no-op export job"),
                ),
            ),
        ];
        let mut export_variants = BTreeSet::new();
        for (exit, expected) in &export_exits {
            let name = match exit {
                ExportStageExit::BridgeUnavailable => "BridgeUnavailable",
                ExportStageExit::SourceRejected(_) => "SourceRejected",
                ExportStageExit::CoverageFailed(_) => "CoverageFailed",
                ExportStageExit::InflightOnly => "InflightOnly",
                ExportStageExit::CoveredReplayFailed(_) => "CoveredReplayFailed",
                ExportStageExit::CoveredNoop { .. } => "CoveredNoop",
            };
            export_variants.insert(name);
            assert_eq!(
                &export_stage_lease_disposition(exit, session),
                expected,
                "{name}"
            );
        }
        assert_eq!(export_variants.len(), 6, "every ExportStageExit variant");

        let (_dir, conn) = ingest_fresh_conn().await;
        let (reservation, request) = sample_reservation(&conn).await;
        let checkpoint = "checkpoint-mapping";
        let checkpoint_log = LeaseLogContext::Checkpoint {
            checkpoint_id: checkpoint,
            message: "failed to release export job after checkpoint write",
        };
        let dirty = (
            LeaseDisposition::Dirty,
            "release_export_job_after_checkpoint_failed",
            checkpoint_log,
        );
        let checkpoint_exits = [
            (
                CheckpointStageExit::UncommittedSettle {
                    catalog_reservation: Box::new(reservation),
                    catalog_request: Box::new(request),
                },
                dirty,
            ),
            (CheckpointStageExit::Uncommitted(Ok(())), dirty),
            (CheckpointStageExit::PostCheckpoint(Ok(())), dirty),
            (
                CheckpointStageExit::Authorized,
                (
                    LeaseDisposition::AdvanceAndRelease,
                    "settle_completed_export_job_recovery_failed",
                    LeaseLogContext::Session {
                        session_id: session,
                        message: "failed to retire timed-out completed export job",
                    },
                ),
            ),
            (
                CheckpointStageExit::Expire(anyhow!("unclassified")),
                (
                    LeaseDisposition::LeaveToExpiry,
                    "export_job_left_to_expiry",
                    LeaseLogContext::Checkpoint {
                        checkpoint_id: checkpoint,
                        message: "export job lease left to expire",
                    },
                ),
            ),
        ];
        let mut checkpoint_variants = BTreeSet::new();
        for (exit, expected) in &checkpoint_exits {
            let name = match exit {
                CheckpointStageExit::UncommittedSettle { .. } => "UncommittedSettle",
                CheckpointStageExit::Uncommitted(_) => "Uncommitted",
                CheckpointStageExit::PostCheckpoint(_) => "PostCheckpoint",
                CheckpointStageExit::Authorized => "Authorized",
                CheckpointStageExit::Expire(_) => "Expire",
            };
            checkpoint_variants.insert(name);
            assert_eq!(
                &checkpoint_stage_lease_disposition(exit, session, checkpoint),
                expected,
                "{name}"
            );
        }
        assert_eq!(
            checkpoint_variants.len(),
            5,
            "every CheckpointStageExit variant"
        );

        // The recount table is executable: each recounted exit's variant
        // maps to the disposition its ADR class requires.
        let disposition_of = |variant: &str| -> LeaseDisposition {
            export_exits
                .iter()
                .find(|(exit, _)| {
                    matches!(
                        (variant, exit),
                        ("BridgeUnavailable", ExportStageExit::BridgeUnavailable)
                            | ("SourceRejected", ExportStageExit::SourceRejected(_))
                            | ("CoverageFailed", ExportStageExit::CoverageFailed(_))
                            | ("InflightOnly", ExportStageExit::InflightOnly)
                            | (
                                "CoveredReplayFailed",
                                ExportStageExit::CoveredReplayFailed(_)
                            )
                            | ("CoveredNoop", ExportStageExit::CoveredNoop { .. })
                    )
                })
                .map(|(_, (disposition, _, _))| *disposition)
                .or_else(|| {
                    checkpoint_exits
                        .iter()
                        .find(|(exit, _)| {
                            matches!(
                                (variant, exit),
                                (
                                    "UncommittedSettle",
                                    CheckpointStageExit::UncommittedSettle { .. }
                                ) | ("Uncommitted", CheckpointStageExit::Uncommitted(_))
                                    | ("PostCheckpoint", CheckpointStageExit::PostCheckpoint(_))
                                    | ("Authorized", CheckpointStageExit::Authorized)
                                    | ("Expire", CheckpointStageExit::Expire(_))
                            )
                        })
                        .map(|(_, (disposition, _, _))| *disposition)
                })
                .unwrap_or_else(|| panic!("recount names an unknown variant {variant}"))
        };
        for (exit, class, variant, helper, disposition) in ENTRY_RECOUNT {
            assert_eq!(
                disposition_of(variant),
                *disposition,
                "{exit} ({class}) via {variant}/{helper}"
            );
        }
        let recounted: BTreeSet<&str> = ENTRY_RECOUNT.iter().map(|row| row.2).collect();
        assert_eq!(
            recounted,
            export_variants
                .union(&checkpoint_variants)
                .copied()
                .collect::<BTreeSet<_>>(),
            "every exit variant is reached by at least one recounted exit"
        );
    }

    /// ACF-20 VER1, drop check: an admitted runner dropped without `settle`
    /// panics in debug builds, while a settled runner and a runner dropped
    /// during an unrelated unwind do not.
    #[cfg(debug_assertions)]
    #[tokio::test]
    async fn export_stage_releases_runner_lease_on_every_exit_unsettled_token_panics() {
        let lease = FakeExportLease::default();
        let scope = CaptureScope {
            repo_id: "export-runner-drop-check".to_string(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let target = LiveExportTarget {
            agent_kind: "opencode",
            provider_session_id: "drop-check",
            scope: &scope,
        };
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30))
            .expect("establish drop-check deadline");
        let admit = || async {
            LiveExportRunner::admit(&lease, target, deadline)
                .await
                .expect("admit fake runner")
                .expect("fake lease elects a runner")
        };

        // Settled exactly once: no panic.
        admit()
            .await
            .settle(
                LeaseDisposition::LeaveToExpiry,
                "export_job_left_to_expiry",
                LeaseLogContext::Session {
                    session_id: "opencode__drop-check",
                    message: "unused",
                },
            )
            .await
            .expect("leave-to-expiry settlement never fails");

        // Dropped during an unrelated unwind: the original panic survives.
        let held = admit().await;
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _held = held;
            panic!("unrelated assertion failure");
        }))
        .expect_err("the unrelated panic propagates");
        assert_eq!(
            unwound.downcast_ref::<&str>().copied(),
            Some("unrelated assertion failure"),
            "an unsettled token must not replace an in-flight panic"
        );

        // Dropped unsettled outside an unwind: the debug drop check fires.
        let unsettled = admit().await;
        let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            drop(unsettled);
        }))
        .expect_err("an unsettled runner token must panic on drop in debug builds");
        assert_eq!(
            dropped.downcast_ref::<&str>().copied(),
            Some("an export runner lease was dropped without settlement")
        );
        assert_eq!(
            lease.calls(),
            [LeaseCall::ObserveIdle; 3],
            "no drop path may touch the lease"
        );
    }

    /// A failed release is logged with the site's fixed reason and fields;
    /// the release outcome never fails the caller.
    #[tokio::test]
    async fn export_stage_releases_runner_lease_on_every_exit_release_warning_fields() {
        use std::io::Write;

        #[derive(Clone, Default)]
        struct Capture(std::sync::Arc<Mutex<Vec<u8>>>);

        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("capture buffer")
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::WARN)
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);

        let lease = FakeExportLease {
            fail_release: true,
            fail_advance: true,
            ..FakeExportLease::default()
        };
        let scope = CaptureScope {
            repo_id: "export-runner-warning-fields".to_string(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30))
            .expect("establish warning-field deadline");
        let session = "opencode__warning-fields";
        for (exit, expect_err) in [
            (ExportStageExit::BridgeUnavailable, false),
            (ExportStageExit::InflightOnly, false),
            (
                ExportStageExit::CoveredNoop {
                    covered_terminal_replay: false,
                },
                true,
            ),
        ] {
            let runner = LiveExportRunner::admit(
                &lease,
                LiveExportTarget {
                    agent_kind: "opencode",
                    provider_session_id: "warning-fields",
                    scope: &scope,
                },
                deadline,
            )
            .await
            .expect("admit fake runner")
            .expect("fake lease elects a runner");
            let (disposition, reason, log) = export_stage_lease_disposition(&exit, session);
            let settled = runner.settle(disposition, reason, log).await;
            assert_eq!(settled.is_err(), expect_err, "{reason}");
        }
        let runner = LiveExportRunner::admit(
            &lease,
            LiveExportTarget {
                agent_kind: "opencode",
                provider_session_id: "warning-fields",
                scope: &scope,
            },
            deadline,
        )
        .await
        .expect("admit fake runner")
        .expect("fake lease elects a runner");
        let (disposition, reason, log) = checkpoint_stage_lease_disposition(
            &CheckpointStageExit::PostCheckpoint(Ok(())),
            session,
            "checkpoint-warning-fields",
        );
        runner
            .settle(disposition, reason, log)
            .await
            .expect("a dirty release never fails its caller");

        let output = String::from_utf8(capture.0.lock().expect("capture buffer").clone())
            .expect("tracing output is UTF-8");
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 4, "one warning per failed release: {output}");
        // The message and the fields, in each site's pre-ACF-20 order.
        for (line, (message, fields)) in lines.iter().zip([
            (
                "failed to release unsuccessful export job",
                [
                    "reason=\"release_failed_export_runner_failed\"",
                    "session_id=***",
                ],
            ),
            (
                "failed to release export job after in-flight skip",
                [
                    "reason=\"release_export_job_after_inflight_skip_failed\"",
                    "session_id=***",
                ],
            ),
            (
                "failed to retire timed-out no-op export job",
                [
                    "reason=\"settle_noop_export_job_recovery_failed\"",
                    "session_id=***",
                ],
            ),
            (
                "failed to release export job after checkpoint write",
                [
                    "checkpoint_id=checkpoint-warning-fields",
                    "reason=\"release_export_job_after_checkpoint_failed\"",
                ],
            ),
        ]) {
            assert!(line.contains(" WARN "), "{line}");
            assert!(line.contains(message), "{line} must carry `{message}`");
            let first = line
                .find(fields[0])
                .unwrap_or_else(|| panic!("{line} must carry `{}`", fields[0]));
            let second = line
                .find(fields[1])
                .unwrap_or_else(|| panic!("{line} must carry `{}`", fields[1]));
            assert!(first < second, "{line}: fields keep their order");
        }
        assert_eq!(
            lease.calls(),
            [
                LeaseCall::ObserveIdle,
                LeaseCall::ReleaseFailed,
                LeaseCall::ObserveIdle,
                LeaseCall::ReleaseDirty,
                LeaseCall::ObserveIdle,
                LeaseCall::AdvanceAndRelease,
                LeaseCall::ReleaseDirty,
                LeaseCall::ObserveIdle,
                LeaseCall::ReleaseDirty,
            ]
        );
    }

    /// The end-to-end half: the checkpoint object I/O of an export always
    /// runs under its exporter-derived deadline and therefore needs the
    /// Unix-only private helper, like the live checkpoint oracle.
    #[cfg(unix)]
    mod end_to_end {
        use std::path::{Path, PathBuf};

        use super::*;
        use crate::internal::ai::{
            hooks::providers::opencode_provider,
            observed_agents::{
                ExportAuthorized, NormalizedTurn, RedactedBytes, TranscriptSource,
                live_capture::{
                    LiveCoverageNormalizer, LiveTranscriptExporter,
                    test_support::with_transcript_exporter,
                },
                normalize_opencode_export,
            },
        };

        /// One coverage-parseable OpenCode turn (`msg_u1`).
        const EXPORT_V1: &str = r#"{"info":{"id":"ses_lease"},"messages":[{"info":{"id":"msg_u1","role":"user"},"parts":[{"type":"text","text":"summarize the lease contract"}]},{"info":{"id":"msg_a1","role":"assistant"},"parts":[{"type":"text","text":"every runner settles once"}]}]}"#;

        /// A different single turn, so a fresh delivery reserves new claims.
        const EXPORT_V2: &str = r#"{"info":{"id":"ses_lease"},"messages":[{"info":{"id":"msg_u9","role":"user"},"parts":[{"type":"text","text":"another question"}]},{"info":{"id":"msg_a9","role":"assistant"},"parts":[{"type":"text","text":"another answer"}]}]}"#;

        /// Actual typed capture and durable object/catalog assertions with a
        /// synthetic exporter; this is not a real OpenCode/native-origin gate.
        #[tokio::test(flavor = "current_thread")]
        #[serial(cwd, env)]
        async fn opencode_extraction_failure_continues_checkpoint() {
            const PARTIAL: &str = r#"{"info":{"id":"ses_og14","location":{"directory":"/project"}},"messages":[{"info":{"id":"msg_og14","role":"user"},"parts":[{"type":"text","text":"human"}]},{"info":{"role":"assistant","modelID":"fixture-model","tokens":{"input":"payload-private-fixture","output":3,"reasoning":0,"cache":{"read":0,"write":0}}},"parts":[{"type":"text","text":"answer"}]}]}"#;
            let fixture = export_fixture().await;
            let session = "og14-extraction";
            start_session(&fixture, session).await;
            deliver(
                &fixture,
                &FakeExportLease::default(),
                ScriptedExport::Authorized(PARTIAL),
                ProviderHookCommand::Stop,
                session,
                "og14-first",
                120_000,
            )
            .await
            .expect("noncritical extraction failure must not block checkpoint persistence");
            assert_eq!(checkpoint_count(&fixture.conn, session).await, 1);
            let row=fixture.conn.query_one_raw(Statement::from_sql_and_values(fixture.conn.get_database_backend(),"SELECT checkpoint_id, metadata_blob_oid, tree_oid FROM agent_checkpoint WHERE session_id = ?",[format!("opencode__{session}").into()])).await.unwrap().expect("real persisted checkpoint row");
            let id: String = row.try_get_by("checkpoint_id").unwrap();
            let oid: String = row.try_get_by("metadata_blob_oid").unwrap();
            let tree: String = row.try_get_by("tree_oid").unwrap();
            let read_blob = |oid: &str| {
                assert!(oid.len() >= 4 && oid.bytes().all(|b| b.is_ascii_hexdigit()));
                let raw =
                    std::fs::read(fixture.repo.join("objects").join(&oid[..2]).join(&oid[2..]))
                        .expect("read actual stored metadata blob");
                let mut inflated = Vec::new();
                std::io::Read::read_to_end(
                    &mut flate2::read::ZlibDecoder::new(raw.as_slice()),
                    &mut inflated,
                )
                .unwrap();
                let nul = inflated
                    .iter()
                    .position(|b| *b == 0)
                    .expect("loose object header");
                inflated.split_off(nul + 1)
            };
            let before = read_blob(&oid);
            let metadata: serde_json::Value = serde_json::from_slice(&before).unwrap();
            assert_eq!(metadata["extraction"]["present"], true);
            assert_eq!(metadata["extraction"]["partial"], true);
            let warnings = metadata["extraction"]["warnings"]
                .as_array()
                .expect("durable warnings");
            assert!(!warnings.is_empty());
            assert!(
                !serde_json::to_string(warnings)
                    .unwrap()
                    .contains("payload-private-fixture")
            );
            deliver(
                &fixture,
                &FakeExportLease::default(),
                ScriptedExport::Authorized("{broken payload-private-fixture"),
                ProviderHookCommand::Stop,
                session,
                "og14-broken",
                120_000,
            )
            .await
            .expect("invalid JSON is contained without replacing committed data");
            let after=fixture.conn.query_one_raw(Statement::from_sql_and_values(fixture.conn.get_database_backend(),"SELECT metadata_blob_oid, tree_oid FROM agent_checkpoint WHERE checkpoint_id = ?",[id.into()])).await.unwrap().expect("original committed row remains");
            assert_eq!(
                after.try_get_by::<String, _>("metadata_blob_oid").unwrap(),
                oid
            );
            assert_eq!(after.try_get_by::<String, _>("tree_oid").unwrap(), tree);
            assert_eq!(
                read_blob(&oid),
                before,
                "original durable metadata is not replaced by invalid JSON"
            );
        }

        /// What the fake transcript exporter returns.
        #[derive(Clone, Copy)]
        enum ScriptedExport {
            /// The bridge is unavailable.
            Unavailable,
            /// Authorized bytes for this callback's Libra session.
            Authorized(&'static str),
            /// Bytes whose export proof names another session.
            ForeignProof(&'static str),
        }

        struct ScriptedExporter(ScriptedExport);

        #[async_trait::async_trait]
        impl LiveTranscriptExporter for ScriptedExporter {
            async fn export(
                &self,
                context: &LiveCaptureContext<'_>,
                _deadline: Instant,
            ) -> Result<TranscriptSource> {
                let (text, proof_session) = match self.0 {
                    ScriptedExport::Unavailable => bail!("scripted export bridge unavailable"),
                    ScriptedExport::Authorized(text) => (text, context.libra_session_id),
                    ScriptedExport::ForeignProof(text) => (text, "opencode__another-session"),
                };
                let bytes = text.as_bytes().to_vec();
                let auth = ExportAuthorized::issue("opencode", proof_session, &bytes);
                Ok(TranscriptSource::Bytes { bytes, auth })
            }

            fn export_coverage_normalizer(&self) -> LiveCoverageNormalizer {
                scripted_export_coverage
            }
        }

        fn scripted_export_coverage(transcript: &RedactedBytes) -> Vec<NormalizedTurn> {
            normalize_opencode_export(transcript.bytes())
        }

        /// OpenCode's binding with its exporter replaced by `script`.
        fn scripted_binding(script: ScriptedExport) -> LiveCaptureBinding {
            with_transcript_exporter(
                LiveCaptureBinding::resolve(opencode_provider()),
                Box::leak(Box::new(ScriptedExporter(script))),
            )
        }

        /// The unit-test executable registers no private helper, and an export
        /// always runs under its exporter-derived deadline, which routes
        /// checkpoint object I/O through that helper: borrow the `libra` binary
        /// Cargo builds beside this test executable (build it first with
        /// `cargo build --bin libra`, as the live checkpoint oracle requires).
        fn libra_helper_program() -> PathBuf {
            let program = std::env::current_exe()
                .expect("locate the unit-test executable")
                .parent()
                .and_then(Path::parent)
                .map(|profile| profile.join("libra"))
                .expect("unit-test executable lives in a Cargo profile directory");
            assert!(
                program.is_file(),
                "export-stage lease tests need the `libra` binary next to the unit-test executable; \
                 build it first with `cargo build --bin libra`: {}",
                program.display()
            );
            program
        }

        struct ExportFixture {
            _dir: TempDir,
            repo: PathBuf,
            conn: DatabaseConnection,
        }

        async fn export_fixture() -> ExportFixture {
            let (dir, conn) = ingest_fresh_conn().await;
            let repo = dir
                .path()
                .canonicalize()
                .expect("canonicalize export fixture repository");
            std::fs::create_dir_all(repo.join("objects"))
                .expect("create export fixture object storage");
            load_capture_dedup_secret(&repo).expect("seed export fixture source commitment key");
            ExportFixture {
                _dir: dir,
                repo,
                conn,
            }
        }

        /// Deliver one OpenCode callback through the typed pipeline with the
        /// scripted exporter and the fake lease port, under a host deadline.
        async fn deliver(
            fixture: &ExportFixture,
            lease: &FakeExportLease,
            script: ScriptedExport,
            command: ProviderHookCommand,
            session: &str,
            event_id: &str,
            budget_millis: u64,
        ) -> Result<()> {
            let hook_event_name = match command {
                ProviderHookCommand::SessionStart => "session.created",
                ProviderHookCommand::Stop => "session.idle",
                ProviderHookCommand::SessionEnd => "session.deleted",
                other => panic!("unexpected OpenCode fixture command {other}"),
            };
            let payload = json!({
                "hook_event_name": hook_event_name,
                "session_id": session,
                "cwd": fixture.repo.to_string_lossy(),
                "event_id": event_id,
            })
            .to_string();
            let deadline = CaptureDeadline::from_budget_millis(budget_millis)
                .expect("establish export fixture host deadline");
            crate::internal::ai::authorized_read::with_test_helper_program(
                libra_helper_program(),
                ingest_agent_traces_payload_with_stable_scope_and_lease(
                    payload.as_bytes(),
                    command,
                    command.lifecycle_event_kind(),
                    opencode_provider(),
                    &fixture.conn,
                    StableScopeIngestOptions {
                        stable_repo_path: &fixture.repo,
                        checkpoint_repo_path: Some(&fixture.repo),
                        deadline: Some(deadline),
                        binding: Some(scripted_binding(script)),
                    },
                    lease,
                ),
            )
            .await
        }

        async fn start_session(fixture: &ExportFixture, session: &str) {
            let lease = FakeExportLease::default();
            deliver(
                fixture,
                &lease,
                ScriptedExport::Unavailable,
                ProviderHookCommand::SessionStart,
                session,
                &format!("{session}-start"),
                30_000,
            )
            .await
            .expect("start export fixture session");
            assert!(
                lease.calls().is_empty(),
                "SessionStart writes no checkpoint and admits no runner"
            );
        }

        async fn checkpoint_count(conn: &DatabaseConnection, session: &str) -> i64 {
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = ?",
                [format!("opencode__{session}").into()],
            ))
            .await
            .expect("count export fixture checkpoints")
            .expect("checkpoint count row")
            .try_get_by::<i64, _>("n")
            .expect("decode checkpoint count")
        }

        async fn claim_states(conn: &DatabaseConnection, session: &str) -> Vec<String> {
            conn.query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state FROM agent_coverage_claim WHERE session_id = ? \
                 ORDER BY logical_turn_key",
                [format!("opencode__{session}").into()],
            ))
            .await
            .expect("read export fixture coverage claims")
            .iter()
            .map(|row| {
                row.try_get_by::<String, _>("state")
                    .expect("decode coverage claim state")
            })
            .collect()
        }

        async fn receipt_statuses(conn: &DatabaseConnection, session: &str) -> Vec<String> {
            let row = conn
                .query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT metadata_json FROM agent_session WHERE provider_session_id = ?",
                    [session.into()],
                ))
                .await
                .expect("read export fixture receipts")
                .expect("export fixture session exists");
            let metadata: serde_json::Value = serde_json::from_str(
                &row.try_get_by::<String, _>("metadata_json")
                    .expect("decode export fixture metadata"),
            )
            .expect("export fixture metadata JSON");
            metadata["capture_catalog_receipts_v1"]["entries"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry["status"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        }

        /// ACF-20 VER1: drive the real export and checkpoint stages with a
        /// scripted `LiveTranscriptExporter` and a fake `LiveExportLeasePort`,
        /// reaching at least one exit of every `ExportStageExit` and
        /// `CheckpointStageExit` variant (see `ENTRY_RECOUNT`). Each admitted
        /// runner is settled exactly once with its class disposition (the debug
        /// drop check would panic otherwise), a failed `AdvanceAndRelease`
        /// retires the lease dirty and skips the continuation, (a) exits abandon
        /// this delivery's claims and (b) exits keep the committed ones.
        #[tokio::test(flavor = "current_thread")]
        async fn export_stage_releases_runner_lease_on_every_exit() {
            use LeaseCall::{AdvanceAndRelease, ObserveIdle, ReleaseDirty, ReleaseFailed};

            use crate::internal::ai::capture::checkpoint::test_support::{
                self as checkpoint_faults, CheckpointFaultPoint,
            };

            // Each successful deliver returns as soon as its stage settles.
            // The full lib suite can still burn a 30 s host budget before the
            // checkpoint write begins, which leaves an armed store fault
            // unfired. The wider budget does not change the deadline-settle
            // case, which passes its own 2 s budget.
            const WIDE: u64 = 120_000;
            let fixture = export_fixture().await;
            let conn = &fixture.conn;

            // Pre-admission: another runner covers this idle; no token exists.
            start_session(&fixture, "recorded-only").await;
            let lease = FakeExportLease {
                recorded_only: true,
                ..FakeExportLease::default()
            };
            deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V1),
                ProviderHookCommand::Stop,
                "recorded-only",
                "recorded-only-idle",
                WIDE,
            )
            .await
            .expect("a recorded-only idle settles its receipt");
            assert_eq!(lease.calls(), [ObserveIdle]);
            assert_eq!(checkpoint_count(conn, "recorded-only").await, 0);

            // BridgeUnavailable → Failed, then a metadata-only checkpoint.
            start_session(&fixture, "bridge-unavailable").await;
            let lease = FakeExportLease::default();
            deliver(
                &fixture,
                &lease,
                ScriptedExport::Unavailable,
                ProviderHookCommand::Stop,
                "bridge-unavailable",
                "bridge-unavailable-idle",
                WIDE,
            )
            .await
            .expect("an unavailable exporter degrades to a metadata-only checkpoint");
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseFailed]);
            assert_eq!(
                lease.targets.lock().expect("fake lease target log")[0],
                ("opencode".to_string(), "bridge-unavailable".to_string()),
                "the runner lease targets the typed kind and the native session"
            );
            assert_eq!(checkpoint_count(conn, "bridge-unavailable").await, 1);
            assert!(claim_states(conn, "bridge-unavailable").await.is_empty());

            // SourceRejected → Failed; no claims, no checkpoint.
            start_session(&fixture, "source-rejected").await;
            let lease = FakeExportLease::default();
            let error = deliver(
                &fixture,
                &lease,
                ScriptedExport::ForeignProof(EXPORT_V1),
                ProviderHookCommand::Stop,
                "source-rejected",
                "source-rejected-idle",
                WIDE,
            )
            .await
            .expect_err("a source that fails its export proof is rejected");
            assert!(
                format!("{error:#}").contains("LBR-AGENT-005: export snapshot is incomplete"),
                "{error:#}"
            );
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseFailed]);
            assert_eq!(checkpoint_count(conn, "source-rejected").await, 0);
            assert!(claim_states(conn, "source-rejected").await.is_empty());

            // InflightOnly → Dirty, then the receipt settles without a checkpoint.
            start_session(&fixture, "inflight-only").await;
            let scope = CaptureScope::main_for_connection(conn)
                .await
                .expect("resolve export fixture scope");
            let mut turns = normalize_opencode_export(EXPORT_V1.as_bytes());
            crate::internal::ai::observed_agents::coverage::redact_turns(&mut turns);
            let held = crate::internal::ai::coverage_gate::reserve_turn_claims_for_channel_with_capture_scope_until(
                conn,
                &scope,
                "opencode__inflight-only",
                &turns,
                "export:other-runner",
                Utc::now().timestamp_millis(),
                crate::internal::ai::coverage_gate::CaptureReservationExecution::export(
                    CaptureCommitDeadline::from_budget(Duration::from_secs(30))
                        .expect("establish competing reservation deadline"),
                ),
            )
            .await
            .expect("another writer holds every turn");
            assert_eq!(held.reserved.len(), 1, "the competing writer owns the turn");
            let lease = FakeExportLease::default();
            deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V1),
                ProviderHookCommand::Stop,
                "inflight-only",
                "inflight-only-idle",
                WIDE,
            )
            .await
            .expect("an in-flight-only skip settles its receipt for a later idle");
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseDirty]);
            assert_eq!(checkpoint_count(conn, "inflight-only").await, 0);

            // Authorized → AdvanceAndRelease; the export claim commits.
            start_session(&fixture, "authorized").await;
            let lease = FakeExportLease::default();
            deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V1),
                ProviderHookCommand::Stop,
                "authorized",
                "authorized-idle-1",
                WIDE,
            )
            .await
            .expect("an authorized export commits its checkpoint");
            assert_eq!(lease.calls(), [ObserveIdle, AdvanceAndRelease]);
            assert_eq!(checkpoint_count(conn, "authorized").await, 1);
            assert_eq!(
                claim_states(conn, "authorized").await,
                ["catalog_committed"]
            );

            // CoveredNoop → AdvanceAndRelease, then the receipt settles.
            let lease = FakeExportLease::default();
            deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V1),
                ProviderHookCommand::Stop,
                "authorized",
                "authorized-idle-2",
                WIDE,
            )
            .await
            .expect("a covered no-op advances the generation and settles");
            assert_eq!(lease.calls(), [ObserveIdle, AdvanceAndRelease]);
            assert_eq!(checkpoint_count(conn, "authorized").await, 1);
            let pending_before = receipt_statuses(conn, "authorized")
                .await
                .iter()
                .filter(|status| *status == "pending")
                .count();

            // CoveredNoop whose advance fails → lease-only Dirty; the catalog
            // continuation is skipped, so this receipt stays pending.
            let lease = FakeExportLease {
                fail_advance: true,
                ..FakeExportLease::default()
            };
            let error = deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V1),
                ProviderHookCommand::Stop,
                "authorized",
                "authorized-idle-3",
                WIDE,
            )
            .await
            .expect_err("a failed no-op advance fails the delivery for a retry");
            assert!(
                format!("{error:#}")
                    .contains("settle no-op export job within its capture deadline"),
                "{error:#}"
            );
            assert_eq!(
                lease.calls(),
                [ObserveIdle, AdvanceAndRelease, ReleaseDirty]
            );
            assert_eq!(
                receipt_statuses(conn, "authorized")
                    .await
                    .iter()
                    .filter(|status| *status == "pending")
                    .count(),
                pending_before + 1,
                "the skipped continuation leaves this delivery's receipt pending"
            );

            // CoveredReplayFailed → Dirty: the deadline elapses between the
            // export coverage reservation and the replay settlement.
            let lease = FakeExportLease::default();
            let error = {
                let _delay = test_support::delay_live_coverage_after_reservation(2_100);
                deliver(
                    &fixture,
                    &lease,
                    ScriptedExport::Authorized(EXPORT_V1),
                    ProviderHookCommand::Stop,
                    "authorized",
                    "authorized-idle-4",
                    2_000,
                )
                .await
                .expect_err("a covered replay past its deadline fails closed")
            };
            assert!(
                format!("{error:#}").contains("covered export replay exceeded its deadline"),
                "{error:#}"
            );
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseDirty]);
            assert_eq!(checkpoint_count(conn, "authorized").await, 1);

            // UncommittedSettle → Dirty: the terminal deadline elapses after the
            // export claims were reserved; they are abandoned and the receipt
            // settles as pending inside its grace slice.
            start_session(&fixture, "deadline-settle").await;
            let lease = FakeExportLease::default();
            {
                let _delay = test_support::delay_live_coverage_after_reservation(2_030);
                deliver(
                    &fixture,
                    &lease,
                    ScriptedExport::Authorized(EXPORT_V1),
                    ProviderHookCommand::SessionEnd,
                    "deadline-settle",
                    "deadline-settle-end",
                    2_000,
                )
                .await
                .expect("an expired terminal settles its pending receipt");
            }
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseDirty]);
            assert_eq!(checkpoint_count(conn, "deadline-settle").await, 0);
            assert_eq!(claim_states(conn, "deadline-settle").await, ["abandoned"]);

            // Uncommitted (direct store failure) → Dirty; claims abandoned.
            start_session(&fixture, "store-failure").await;
            let lease = FakeExportLease::default();
            checkpoint_faults::fail_once_at(CheckpointFaultPoint::AfterObjectWrite);
            let error = deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V2),
                ProviderHookCommand::Stop,
                "store-failure",
                "store-failure-idle",
                WIDE,
            )
            .await
            .expect_err("a checkpoint store failure fails the delivery");
            assert_eq!(
                checkpoint_faults::armed_fault(),
                None,
                "the store fault fired; delivery failed earlier: {error:#}"
            );
            assert!(
                format!("{error:#}").contains("capture coordinator execution failed"),
                "{error:#}"
            );
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseDirty]);
            assert_eq!(checkpoint_count(conn, "store-failure").await, 0);
            assert_eq!(claim_states(conn, "store-failure").await, ["abandoned"]);

            // PostCheckpoint → Dirty; the committed companion claim is kept.
            start_session(&fixture, "pending-cleanup").await;
            let lease = FakeExportLease::default();
            checkpoint_faults::fail_once_at(CheckpointFaultPoint::MarkerCleanup);
            let error = deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V2),
                ProviderHookCommand::Stop,
                "pending-cleanup",
                "pending-cleanup-idle",
                WIDE,
            )
            .await
            .expect_err("a durable checkpoint awaiting cleanup fails the delivery");
            assert_eq!(
                checkpoint_faults::armed_fault(),
                None,
                "the cleanup fault fired"
            );
            assert!(
                format!("{error:#}").contains("needs durable cleanup before completion"),
                "{error:#}"
            );
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseDirty]);
            assert_eq!(checkpoint_count(conn, "pending-cleanup").await, 1);
            assert_eq!(
                claim_states(conn, "pending-cleanup").await,
                ["catalog_committed"],
                "(b) keeps the claims the ref-CAS transaction committed"
            );

            // Authorized whose advance fails → lease-only Dirty; the checkpoint
            // stays durable and the delivery fails for a retry.
            start_session(&fixture, "advance-failure").await;
            let lease = FakeExportLease {
                fail_advance: true,
                ..FakeExportLease::default()
            };
            let error = deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V2),
                ProviderHookCommand::Stop,
                "advance-failure",
                "advance-failure-idle",
                WIDE,
            )
            .await
            .expect_err("a failed completed advance fails the delivery");
            assert!(
                format!("{error:#}")
                    .contains("settle completed export job within its capture deadline"),
                "{error:#}"
            );
            assert_eq!(
                lease.calls(),
                [ObserveIdle, AdvanceAndRelease, ReleaseDirty]
            );
            assert_eq!(checkpoint_count(conn, "advance-failure").await, 1);

            // Expire → LeaveToExpiry: an unclassified coordinator error writes
            // no lease state.
            start_session(&fixture, "unclassified").await;
            let lease = FakeExportLease::default();
            crate::internal::ai::capture::coordinator::test_support::interrupt_after_checkpoint_once();
            let error = deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V2),
                ProviderHookCommand::Stop,
                "unclassified",
                "unclassified-idle",
                WIDE,
            )
            .await
            .expect_err("an unclassified coordinator error fails the delivery");
            assert!(
                format!("{error:#}").contains("capture coordinator execution failed"),
                "{error:#}"
            );
            assert_eq!(lease.calls(), [ObserveIdle]);

            // CoverageFailed → Dirty: the export claim gate is unavailable.
            // Last, because it disables coverage for the whole fixture.
            start_session(&fixture, "coverage-failed").await;
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "ALTER TABLE agent_coverage_claim RENAME TO agent_coverage_claim_unavailable"
                    .to_string(),
            ))
            .await
            .expect("make the coverage claim gate unavailable");
            let lease = FakeExportLease::default();
            let error = deliver(
                &fixture,
                &lease,
                ScriptedExport::Authorized(EXPORT_V1),
                ProviderHookCommand::Stop,
                "coverage-failed",
                "coverage-failed-idle",
                WIDE,
            )
            .await
            .expect_err("an unavailable coverage gate fails closed");
            assert!(
                format!("{error:#}").contains(
                    "coverage gate reservation failed; export capture aborted (fail-closed)"
                ),
                "{error:#}"
            );
            assert_eq!(lease.calls(), [ObserveIdle, ReleaseDirty]);
            assert_eq!(checkpoint_count(conn, "coverage-failed").await, 0);
        }
    }
}
