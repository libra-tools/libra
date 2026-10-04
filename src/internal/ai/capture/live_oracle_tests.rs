//! ACF-17 characterization oracle for the durable live checkpoint shape.
//!
//! The live hook runtime is being decomposed across `hooks/runtime.rs`,
//! `hooks/intent.rs` and the `capture::{live, scope_binding, ...}` modules.
//! This test-only module pins the `metadata.json` / `redaction_report.json`
//! bytes a live checkpoint publishes, before and after every extraction step.
//! It drives the runtime exclusively through the public
//! [`crate::internal::ai::capture::test_support`] harness and capture ingress,
//! never through runtime internals, so later cards can move those internals
//! without editing the oracle. Its token stream is fingerprinted by
//! `compat_agent_architecture_guard::capture_runtime_extraction_oracles_are_frozen`.
//!
//! Run prerequisite: the OpenCode leg borrows the `libra` binary Cargo builds
//! beside the unit-test executable, and `cargo test --lib` does not build bin
//! targets. Run the oracle after `cargo build --bin libra`, or under
//! `cargo test --all` / `cargo nextest run`, which build that binary from the
//! same sources. A missing binary fails loudly; a stale one is not detected.

use std::path::{Path, PathBuf};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

use crate::internal::ai::{
    capture::{
        ingress::lower_in_process_capture_frame_for_test,
        test_support::ingest_agent_traces_ingress_outcome_for_test,
    },
    hooks::{
        HookProvider, LifecycleEventKind, ProviderHookCommand,
        providers::{claude_provider, codex_provider, opencode_provider},
    },
};

const ORACLE_BOOTSTRAP_SQL: &str = include_str!("../../../../sql/sqlite_20260309_init.sql");

/// Coverage-parseable Claude transcript with one stable logical turn and a
/// secret the redactor must scrub before it reaches durable metadata.
const ORACLE_CLAUDE_TRANSCRIPT: &str = concat!(
    r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"deploy with AKIAIOSFODNN7EXAMPLE"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
    "\n",
);

/// One observed artifact per line: `<case> <checkpoint#> <scope> <file> <json>`.
/// Only `checkpoint_id`, `created_at`, repository-keyed HMAC digests and the
/// temporary repository path are normalized; every other JSON value is pinned.
const LIVE_CHECKPOINT_METADATA_SHAPE: &str = r##"claude-source 0 committed metadata.json {"agent_kind":"claude_code","extraction":{"aggregate_token_usage":{"input_tokens":0,"output_tokens":0},"api_call_count":0,"modified_files":[],"partial":true,"present":true,"prompt_count":1,"schema_version":1,"skill_events":[],"subagent_source_count":0,"token_usage":{"input_tokens":0,"output_tokens":0},"warnings":["killable subagent content discovery is unavailable in this executable"]},"model":"unknown","provider_session_id":"oracle-claude-source","redaction_report":{"bytes_redacted":20,"bytes_scanned":205,"matches":[{"end":95,"rule_id":"aws-access-key-id","start":75}]},"schema_version":2,"scope":"committed","session_id":"claude__oracle-claude-source","transcript_snapshot":{"completeness":"complete","partial_reason":null,"redaction_bytes_redacted":20,"redaction_bytes_scanned":205,"redaction_match_count":1,"source":{"byte_len":205,"digest_sha256":"source/hmac-v2/<hmac>","identity":"not_retained:v1","kind":"provider_file"},"transcript_redacted_bytes":213},"working_dir":"<REPO>"}
claude-source 0 committed redaction_report.json {"bytes_redacted":20,"bytes_scanned":205,"matches":[{"end":95,"rule_id":"aws-access-key-id","start":75}]}
claude-source 1 committed metadata.json {"agent_kind":"claude_code","extraction":{"aggregate_token_usage":{"input_tokens":0,"output_tokens":0},"api_call_count":0,"modified_files":[],"partial":true,"present":true,"prompt_count":1,"schema_version":1,"skill_events":[],"subagent_source_count":0,"token_usage":{"input_tokens":0,"output_tokens":0},"warnings":["killable subagent content discovery is unavailable in this executable"]},"model":"unknown","provider_session_id":"oracle-claude-source","redaction_report":{"bytes_redacted":20,"bytes_scanned":205,"matches":[{"end":95,"rule_id":"aws-access-key-id","start":75}]},"schema_version":2,"scope":"committed","session_id":"claude__oracle-claude-source","transcript_snapshot":{"completeness":"complete","partial_reason":null,"redaction_bytes_redacted":20,"redaction_bytes_scanned":205,"redaction_match_count":1,"source":{"byte_len":205,"digest_sha256":"source/hmac-v2/<hmac>","identity":"not_retained:v1","kind":"provider_file"},"transcript_redacted_bytes":213},"working_dir":"<REPO>"}
claude-source 1 committed redaction_report.json {"bytes_redacted":20,"bytes_scanned":205,"matches":[{"end":95,"rule_id":"aws-access-key-id","start":75}]}
codex-source-absent 0 committed metadata.json {"agent_kind":"codex","extraction":{"partial":true,"present":false,"schema_version":1,"warnings":["no redacted transcript available; extraction skipped"]},"model":"unknown","provider_session_id":"oracle-codex-source-absent","redaction_report":{"bytes_redacted":0,"bytes_scanned":0,"matches":[]},"schema_version":2,"scope":"committed","session_id":"codex__oracle-codex-source-absent","transcript_snapshot":{"completeness":"partial","partial_reason":"source_absent","redaction_bytes_redacted":0,"redaction_bytes_scanned":0,"redaction_match_count":0,"source":null,"transcript_redacted_bytes":0},"working_dir":"<REPO>"}
codex-source-absent 0 committed redaction_report.json {"bytes_redacted":0,"bytes_scanned":0,"matches":[]}
codex-source-absent 1 committed metadata.json {"agent_kind":"codex","extraction":{"partial":true,"present":false,"schema_version":1,"warnings":["no redacted transcript available; extraction skipped"]},"model":"unknown","provider_session_id":"oracle-codex-source-absent","redaction_report":{"bytes_redacted":0,"bytes_scanned":0,"matches":[]},"schema_version":2,"scope":"committed","session_id":"codex__oracle-codex-source-absent","transcript_snapshot":{"completeness":"partial","partial_reason":"source_absent","redaction_bytes_redacted":0,"redaction_bytes_scanned":0,"redaction_match_count":0,"source":null,"transcript_redacted_bytes":0},"working_dir":"<REPO>"}
codex-source-absent 1 committed redaction_report.json {"bytes_redacted":0,"bytes_scanned":0,"matches":[]}
opencode-bridge-unavailable 0 committed metadata.json {"agent_kind":"opencode","extraction":{"partial":true,"present":false,"schema_version":1,"warnings":["no redacted transcript available; extraction skipped"]},"model":"unknown","provider_session_id":"oracle-opencode-bridge-unavailable","redaction_report":{"bytes_redacted":0,"bytes_scanned":0,"matches":[]},"schema_version":2,"scope":"committed","session_id":"opencode__oracle-opencode-bridge-unavailable","transcript_snapshot":{"completeness":"partial","partial_reason":"source_read_error","redaction_bytes_redacted":0,"redaction_bytes_scanned":0,"redaction_match_count":0,"source":null,"transcript_redacted_bytes":0},"working_dir":"<REPO>"}
opencode-bridge-unavailable 0 committed redaction_report.json {"bytes_redacted":0,"bytes_scanned":0,"matches":[]}
opencode-bridge-unavailable 1 committed metadata.json {"agent_kind":"opencode","extraction":{"partial":true,"present":false,"schema_version":1,"warnings":["no redacted transcript available; extraction skipped"]},"model":"unknown","provider_session_id":"oracle-opencode-bridge-unavailable","redaction_report":{"bytes_redacted":0,"bytes_scanned":0,"matches":[]},"schema_version":2,"scope":"committed","session_id":"opencode__oracle-opencode-bridge-unavailable","transcript_snapshot":{"completeness":"partial","partial_reason":"source_read_error","redaction_bytes_redacted":0,"redaction_bytes_scanned":0,"redaction_match_count":0,"source":null,"transcript_redacted_bytes":0},"working_dir":"<REPO>"}
opencode-bridge-unavailable 1 committed redaction_report.json {"bytes_redacted":0,"bytes_scanned":0,"matches":[]}"##;

/// Isolate provider roots and the global configuration store for the oracle.
/// Drop restores every prior value even when an assertion unwinds, so the
/// serial env lane is never leaked.
struct OracleEnvGuard {
    prior: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl OracleEnvGuard {
    fn set(values: &[(&'static str, PathBuf)]) -> Self {
        let mut prior = Vec::new();
        for (name, value) in values {
            prior.push((*name, std::env::var_os(name)));
            // SAFETY: test-only process environment mutation, restored by
            // Drop; the oracle holds the serial env lane.
            unsafe {
                std::env::set_var(name, value);
            }
        }
        Self { prior }
    }
}

impl Drop for OracleEnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.prior.drain(..).rev() {
            // SAFETY: paired with `set` under the same serial env lane.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// The unit-test executable has no `libra` main and therefore registers no
/// private helper. OpenCode always runs under its exporter-derived deadline,
/// which routes checkpoint object I/O through that helper, so borrow the
/// `libra` binary Cargo builds beside this test executable. It is launched
/// once up front so first-launch OS checks cannot consume the export budget.
fn oracle_helper_program() -> PathBuf {
    let program = std::env::current_exe()
        .expect("locate the unit-test executable")
        .parent()
        .and_then(Path::parent)
        .map(|profile| profile.join("libra"))
        .expect("unit-test executable lives in a Cargo profile directory");
    assert!(
        program.is_file(),
        "the OpenCode leg needs the `libra` binary next to the unit-test executable; \
         build it first with `cargo build --bin libra`: {}",
        program.display()
    );
    let warm = std::process::Command::new(&program)
        .arg("--version")
        .output()
        .expect("launch the libra helper binary");
    assert!(warm.status.success(), "the libra helper binary must run");
    program
}

/// A database-only repository wired exactly like production storage: legacy
/// bootstrap, AI runtime contract, registered migrations, repository identity
/// and an object store for checkpoint objects.
async fn oracle_repository() -> (TempDir, PathBuf, DatabaseConnection) {
    let dir = tempfile::tempdir().expect("create oracle repository");
    let repo = dir
        .path()
        .canonicalize()
        .expect("canonicalize oracle repository");
    std::fs::create_dir_all(repo.join("objects")).expect("create oracle object store");
    let path = repo.join(crate::utils::util::DATABASE);
    std::fs::File::create(&path).expect("create oracle database");
    let mut options = ConnectOptions::new(format!("sqlite://{}", path.display()));
    options.sqlx_logging(false);
    let conn = Database::connect(options)
        .await
        .expect("connect oracle database");
    let backend = conn.get_database_backend();
    for raw in ORACLE_BOOTSTRAP_SQL.split(';') {
        let statement = raw.trim();
        if statement.is_empty() {
            continue;
        }
        conn.execute_raw(Statement::from_string(backend, statement.to_string()))
            .await
            .unwrap_or_else(|error| panic!("oracle bootstrap failed: {statement}\n{error}"));
    }
    crate::internal::db::ensure_ai_runtime_contract_schema(&conn)
        .await
        .expect("ensure oracle AI runtime contract schema");
    crate::internal::db::migration::run_builtin_migrations(&conn)
        .await
        .expect("run oracle migrations");
    crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
        .await
        .expect("seed oracle repository identity");
    (dir, repo, conn)
}

/// Deliver one canonical frame through capture ingress and the public
/// AgentTraces harness.
async fn oracle_deliver(
    conn: &DatabaseConnection,
    repo: &Path,
    provider: &dyn HookProvider,
    command: ProviderHookCommand,
    hook_event_name: &str,
    session_id: &str,
) {
    let payload = json!({
        "hook_event_name": hook_event_name,
        "session_id": session_id,
        "cwd": repo.to_string_lossy(),
        "event_id": format!("oracle-{session_id}-{command}"),
    })
    .to_string();
    let expected_kind: LifecycleEventKind = command.lifecycle_event_kind();
    let outcome = lower_in_process_capture_frame_for_test(
        payload.as_bytes(),
        command,
        expected_kind,
        provider,
        Some(repo),
    );
    ingest_agent_traces_ingress_outcome_for_test(outcome, command, provider, conn, Some(repo))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "oracle {provider_name} {command} delivery failed: {error:#}",
                provider_name = provider.provider_name()
            )
        });
}

/// Read one loose object and return its body after the `<type> <len>\0` header.
fn oracle_object_body(repo: &Path, oid: &str) -> Vec<u8> {
    let raw = std::fs::read(repo.join("objects").join(&oid[..2]).join(&oid[2..]))
        .unwrap_or_else(|error| panic!("read oracle object {oid}: {error}"));
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::ZlibDecoder::new(raw.as_slice()),
        &mut decoded,
    )
    .unwrap_or_else(|error| panic!("inflate oracle object {oid}: {error}"));
    let header_end = decoded
        .iter()
        .position(|byte| *byte == 0)
        .expect("oracle object has a header terminator");
    decoded.split_off(header_end + 1)
}

/// Resolve one `/`-separated path below a SHA-1 tree to its blob body.
fn oracle_tree_blob(repo: &Path, tree_oid: &str, path: &str) -> Vec<u8> {
    let mut oid = tree_oid.to_string();
    for component in path.split('/') {
        let body = oracle_object_body(repo, &oid);
        let mut cursor = 0;
        let mut found = None;
        while cursor < body.len() {
            let space = cursor
                + body[cursor..]
                    .iter()
                    .position(|byte| *byte == b' ')
                    .expect("oracle tree entry mode terminator");
            let nul = space
                + 1
                + body[space + 1..]
                    .iter()
                    .position(|byte| *byte == 0)
                    .expect("oracle tree entry name terminator");
            let name = &body[space + 1..nul];
            let child = hex::encode(&body[nul + 1..nul + 21]);
            if name == component.as_bytes() {
                found = Some(child);
                break;
            }
            cursor = nul + 21;
        }
        oid = found.unwrap_or_else(|| panic!("oracle tree lacks {path}"));
    }
    oracle_object_body(repo, &oid)
}

/// Remove only the per-write identity/time fields, repository-keyed HMAC
/// digests and the temporary repository path; every other value is part of
/// the pinned shape.
fn oracle_normalize(value: &mut Value, repo: &str) {
    match value {
        Value::Object(map) => {
            map.remove("checkpoint_id");
            map.remove("created_at");
            for child in map.values_mut() {
                oracle_normalize(child, repo);
            }
        }
        Value::Array(items) => {
            for item in items {
                oracle_normalize(item, repo);
            }
        }
        Value::String(text) => {
            if text.contains(repo) {
                *text = text.replace(repo, "<REPO>");
            }
            if let Some((prefix, digest)) = text.rsplit_once('/')
                && prefix.contains("hmac")
                && digest.len() == 64
                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                *text = format!("{prefix}/<hmac>");
            }
        }
        _ => {}
    }
}

/// ACF-17 VER: `metadata.json` and `redaction_report.json` of every live
/// checkpoint for a Claude session with an authorized source, a Codex session
/// without one, and an OpenCode session whose export bridge is unavailable.
#[tokio::test(flavor = "current_thread")]
#[serial(cwd, env)]
async fn live_checkpoint_metadata_shape_is_stable() {
    let home = tempfile::tempdir().expect("create oracle provider home");
    let _env = OracleEnvGuard::set(&[
        ("LIBRA_TEST_HOME", home.path().to_path_buf()),
        (
            "LIBRA_CONFIG_GLOBAL_DB",
            home.path().join("oracle-global-config.db"),
        ),
    ]);
    let cases: [(&str, &dyn HookProvider, [&str; 3]); 3] = [
        (
            "claude-source",
            claude_provider(),
            ["SessionStart", "Stop", "SessionEnd"],
        ),
        (
            "codex-source-absent",
            codex_provider(),
            ["SessionStart", "Stop", "SessionEnd"],
        ),
        (
            "opencode-bridge-unavailable",
            opencode_provider(),
            ["session.created", "session.idle", "session.deleted"],
        ),
    ];
    let mut observed = Vec::new();
    for (label, provider, events) in cases {
        let (_dir, repo, conn) = oracle_repository().await;
        // Capture helpers that resolve repository storage from the process
        // cwd must see this oracle's fixture, not the developer checkout (or
        // GitHub's plain .git checkout, which has no .libra directory).
        let _cwd = crate::utils::test::ChangeDirGuard::new(&repo);
        let session_id = format!("oracle-{label}");
        if label == "claude-source" {
            let source_dir = crate::internal::ai::observed_agents::claude_session_dir(&repo)
                .expect("oracle home provides a Claude project directory");
            std::fs::create_dir_all(&source_dir).expect("create oracle Claude project");
            std::fs::write(
                source_dir.join(format!("{session_id}.jsonl")),
                ORACLE_CLAUDE_TRANSCRIPT,
            )
            .expect("write oracle Claude transcript");
        }
        let deliveries = async {
            for (command, hook_event_name) in [
                ProviderHookCommand::SessionStart,
                ProviderHookCommand::Stop,
                ProviderHookCommand::SessionEnd,
            ]
            .into_iter()
            .zip(events)
            {
                oracle_deliver(
                    &conn,
                    &repo,
                    provider,
                    command,
                    hook_event_name,
                    &session_id,
                )
                .await;
            }
        };
        if label == "opencode-bridge-unavailable" {
            crate::internal::ai::authorized_read::with_test_helper_program(
                oracle_helper_program(),
                deliveries,
            )
            .await;
        } else {
            deliveries.await;
        }
        crate::utils::client_storage::ClientStorage::wait_for_background_tasks();
        let rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT checkpoint_id, scope, tree_oid, metadata_blob_oid \
                 FROM agent_checkpoint ORDER BY created_at, rowid"
                    .to_string(),
            ))
            .await
            .expect("query oracle checkpoints");
        for (index, row) in rows.iter().enumerate() {
            let checkpoint_id: String = row.try_get_by("checkpoint_id").expect("checkpoint id");
            let scope: String = row.try_get_by("scope").expect("checkpoint scope");
            let tree_oid: String = row.try_get_by("tree_oid").expect("checkpoint tree");
            let metadata_oid: String = row
                .try_get_by("metadata_blob_oid")
                .expect("checkpoint metadata blob");
            let leaf = format!("checkpoint/{}/{}", &checkpoint_id[..2], &checkpoint_id[2..]);
            for (file, bytes) in [
                ("metadata.json", oracle_object_body(&repo, &metadata_oid)),
                (
                    "redaction_report.json",
                    oracle_tree_blob(&repo, &tree_oid, &format!("{leaf}/redaction_report.json")),
                ),
            ] {
                let mut document: Value = serde_json::from_slice(&bytes)
                    .unwrap_or_else(|error| panic!("{label} {file} is not JSON: {error}"));
                oracle_normalize(&mut document, &repo.to_string_lossy());
                observed.push(format!("{label} {index} {scope} {file} {document}"));
            }
        }
    }
    let expected: Vec<&str> = LIVE_CHECKPOINT_METADATA_SHAPE.lines().collect();
    assert_eq!(
        observed,
        expected,
        "live checkpoint metadata shape drifted; observed table:\n{}",
        observed.join("\n")
    );
}
