//! AG-24a raw-access gate for `libra agent checkpoint export` (plan.md
//! Task A8.5): redacted export needs no authorization; a raw export
//! requires `--allow-raw`, a raw request without it is refused fail-closed
//! (`LBR-AGENT-013`), and every raw access (grant or deny) appends one
//! append-only `agent_audit_log` row.

use std::{path::Path, sync::Arc, time::Duration};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use libra::{
    internal::{
        ai::{
            history::{
                CheckpointCommitParams, CheckpointScope, HistoryManager, TracesInflightMarker,
                clear_traces_inflight_marker_if_generation, write_traces_inflight_marker,
            },
            observed_agents::Redactor,
        },
        branch::TRACES_BRANCH,
    },
    utils::{
        client_storage::ClientStorage,
        object::{read_git_object, write_git_object},
    },
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{init_repo_via_cli, run_libra_command};

const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts).await.expect("connect repo db")
}

/// Seed a stopped session plus a real E4-libra checkpoint whose transcript
/// embeds `SECRET`, and return the checkpoint id.
async fn seed_checkpoint_with_secret(repo: &Path) -> String {
    let conn = connect_repo_db(repo).await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES ('sess-x', 'claude_code', 'p-x', 'stopped', '/tmp/x', '{}', '{}', 1, 2, 3)",
        Vec::<Value>::new(),
    ))
    .await
    .expect("insert session");

    let repo_path = repo.join(".libra");
    let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
    let history =
        HistoryManager::new_with_ref(storage, repo_path, Arc::new(conn.clone()), TRACES_BRANCH);
    let redactor = Redactor::new_default();
    let (redacted, _) = redactor.redact(format!("transcript with key {SECRET} inside").as_bytes());
    let (meta_redacted, _) = redactor.redact(br#"{"checkpoint_id":"x"}"#);
    let (events_redacted, _) = redactor.redact(b"{}\n");
    let (report_redacted, _) = redactor.redact(b"{}");
    let checkpoint_id = "aabbccddeeff00112233445566778899".to_string();
    let marker = TracesInflightMarker::new(
        "sess-x",
        &checkpoint_id,
        chrono::Utc::now().timestamp_millis(),
    );
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("register seeded checkpoint writer marker");
    let written = history
        .append_checkpoint_commit(CheckpointCommitParams {
            reasoning_artifacts: &[],
            checkpoint_id: &checkpoint_id,
            session_id: "sess-x",
            marker_generation: marker.generation.as_deref().expect("new marker generation"),
            capture_scope: None,
            agent_kind: "claude_code",
            parent_commit: None,
            scope: CheckpointScope::Committed,
            tool_use_id: None,
            metadata_json: &meta_redacted,
            transcript_redacted: &redacted,
            lifecycle_events_jsonl: &events_redacted,
            redaction_report_json: &report_redacted,
            txn_extra: None,
            deadline: None,
        })
        .await
        .expect("append checkpoint");

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, 'sess-x', 'committed', NULL, ?, ?, ?, 100)",
        vec![
            Value::from(checkpoint_id.clone()),
            Value::from(written.tree_oid.to_string()),
            Value::from(written.metadata_blob_oid.to_string()),
            Value::from(written.commit_hash.to_string()),
        ],
    ))
    .await
    .expect("insert checkpoint row");
    clear_traces_inflight_marker_if_generation(
        &conn,
        "sess-x",
        &checkpoint_id,
        &written.marker_generation,
    )
    .await
    .expect("retire seeded checkpoint writer marker");
    conn.close().await.expect("close seed conn");
    checkpoint_id
}

async fn audit_rows(repo: &Path) -> Vec<(String, i64)> {
    let conn = connect_repo_db(repo).await;
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT checkpoint_id, granted FROM agent_audit_log ORDER BY timestamp".to_string(),
        ))
        .await
        .expect("query audit");
    rows.into_iter()
        .map(|r| {
            (
                r.try_get_by::<String, _>("checkpoint_id").unwrap(),
                r.try_get_by::<i64, _>("granted").unwrap(),
            )
        })
        .collect()
}

/// Full gate: redacted export needs no auth and writes no audit; a raw
/// request without --allow-raw is refused (LBR-AGENT-013) + audited as a
/// denial; --allow-raw --raw grants + audits + returns the un-redacted
/// bytes.
#[tokio::test]
async fn allow_raw_gate() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let checkpoint_id = seed_checkpoint_with_secret(repo.path()).await;

    // (1) Default redacted export: succeeds, no audit row. The stored
    // transcript is redacted at capture (P0), so the secret is already
    // gone from every path — the redacted export re-scrubs defensively.
    let out = run_libra_command(
        &["agent", "checkpoint", "export", &checkpoint_id],
        repo.path(),
    );
    assert!(out.status.success(), "redacted export must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("transcript with key"),
        "redacted export returns the (capture-redacted) transcript body: {stdout}"
    );
    assert!(!stdout.contains(SECRET), "the secret is never present");
    assert!(
        audit_rows(repo.path()).await.is_empty(),
        "redacted path writes no audit"
    );

    // (2) Raw request WITHOUT --allow-raw: fail-closed + audited denial.
    let out = run_libra_command(
        &["agent", "checkpoint", "export", &checkpoint_id, "--raw"],
        repo.path(),
    );
    assert!(!out.status.success(), "raw without --allow-raw must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-013"),
        "denial carries the stable code: {stderr}"
    );
    let rows = audit_rows(repo.path()).await;
    assert_eq!(rows.len(), 1, "the refusal is audited");
    assert_eq!(
        rows[0],
        (checkpoint_id.clone(), 0),
        "denial recorded granted=0"
    );

    // (3) --allow-raw --raw: granted + audited + un-redacted bytes.
    let out = run_libra_command(
        &[
            "agent",
            "checkpoint",
            "export",
            &checkpoint_id,
            "--allow-raw",
            "--raw",
        ],
        repo.path(),
    );
    assert!(out.status.success(), "authorized raw export must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("transcript with key"),
        "raw export returns the stored transcript body as-is: {stdout}"
    );
    assert!(
        !stdout.contains(SECRET),
        "even raw export cannot leak a secret redacted at capture (P0)"
    );
    let rows = audit_rows(repo.path()).await;
    assert_eq!(rows.len(), 2, "the grant is audited too");
    assert_eq!(rows[1], (checkpoint_id, 1), "grant recorded granted=1");
}

/// The raw gate is fail-closed BEFORE the checkpoint lookup: `export
/// <missing-id> --raw` returns LBR-AGENT-013 (not a "no checkpoint" error
/// that would leak an existence oracle) and audits the refusal.
#[tokio::test]
async fn raw_denial_precedes_lookup_no_existence_oracle() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    // Deliberately do NOT seed the checkpoint.
    let out = run_libra_command(
        &[
            "agent",
            "checkpoint",
            "export",
            "deadbeefdeadbeefdeadbeefdeadbeef",
            "--raw",
        ],
        repo.path(),
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-013"),
        "denial fires before lookup, not a 'no checkpoint' error: {stderr}"
    );
    assert!(
        !stderr.contains("no checkpoint matches"),
        "must not reveal whether the id exists: {stderr}"
    );
    let rows = audit_rows(repo.path()).await;
    assert_eq!(rows.len(), 1, "the pre-lookup refusal is still audited");
    assert_eq!(rows[0].1, 0, "recorded as a denial");
}

/// `--allow-raw` on its own (without `--raw`) is NOT a raw export: it
/// falls through to the redacted path and writes no audit row.
#[tokio::test]
async fn allow_raw_without_raw_is_redacted_no_audit() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let checkpoint_id = seed_checkpoint_with_secret(repo.path()).await;
    let out = run_libra_command(
        &[
            "agent",
            "checkpoint",
            "export",
            &checkpoint_id,
            "--allow-raw",
        ],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "--allow-raw alone must succeed (redacted)"
    );
    assert!(
        audit_rows(repo.path()).await.is_empty(),
        "--allow-raw without --raw takes the redacted path and writes no audit"
    );
}

const ARTIFACT_CANARY: &[u8] = br#"opaque-signature-keep-\\u0041-byte-exact"#;

fn load_fixture_tree(storage: &Path, oid: ObjectHash) -> Vec<TreeItem> {
    Tree::from_bytes(
        &read_git_object(storage, &oid).expect("read fixture tree"),
        oid,
    )
    .expect("parse fixture tree")
    .tree_items
}

fn write_fixture_tree(storage: &Path, mut items: Vec<TreeItem>) -> ObjectHash {
    items.sort_by(|a, b| a.name.cmp(&b.name));
    let tree = Tree::from_tree_items(items).expect("build fixture tree");
    write_git_object(
        storage,
        "tree",
        &tree.to_data().expect("encode fixture tree"),
    )
    .expect("write fixture tree")
}

fn artifact_object_path(repo: &Path, oid: &str) -> std::path::PathBuf {
    repo.join(".libra/objects").join(&oid[..2]).join(&oid[2..])
}

async fn seed_temporary_gc_checkpoint(repo: &Path) -> (String, ObjectHash) {
    let conn = connect_repo_db(repo).await;
    let storage = repo.join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(storage.join("objects"))),
        storage,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let id = uuid::Uuid::new_v4().to_string();
    let marker = TracesInflightMarker::new("sess-x", &id, chrono::Utc::now().timestamp_millis());
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("register temporary GC checkpoint");
    let redactor = Redactor::new_default();
    let metadata = serde_json::to_vec(&json!({
        "checkpoint_id": id, "session_id": "sess-x", "agent_kind": "claude_code",
        "scope": "temporary", "created_at": 100,
    }))
    .expect("temporary GC metadata");
    let (metadata, _) = redactor.redact(&metadata);
    let (transcript, _) = redactor.redact(b"temporary checkpoint GC control");
    let (events, _) = redactor.redact(b"{}\n");
    let (report, _) = redactor.redact(b"{}");
    let written = history
        .append_checkpoint_commit(CheckpointCommitParams {
            checkpoint_id: &id,
            session_id: "sess-x",
            marker_generation: marker.generation.as_deref().expect("temporary generation"),
            capture_scope: None,
            agent_kind: "claude_code",
            parent_commit: None,
            scope: CheckpointScope::Temporary,
            tool_use_id: None,
            metadata_json: &metadata,
            transcript_redacted: &transcript,
            lifecycle_events_jsonl: &events,
            redaction_report_json: &report,
            txn_extra: None,
            deadline: None,
            reasoning_artifacts: &[],
        })
        .await
        .expect("append temporary GC checkpoint");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES (?, 'sess-x', 'temporary', ?, ?, ?, 100)",
        [id.clone().into(), written.tree_oid.to_string().into(), written.metadata_blob_oid.to_string().into(), written.commit_hash.to_string().into()],
    ))
    .await
    .expect("catalog temporary GC checkpoint");
    clear_traces_inflight_marker_if_generation(&conn, "sess-x", &id, &written.marker_generation)
        .await
        .expect("retire temporary GC marker");
    assert!(
        ClientStorage::wait_for_background_tasks_until(
            std::time::Instant::now() + Duration::from_secs(10)
        )
        .await,
        "temporary GC fixture indexing must settle"
    );
    conn.close().await.expect("close temporary GC database");
    (id, written.commit_hash)
}

/// Exercise both quarantine scans in an isolated fixture without waiting an
/// hour. Only the fixture object mtimes and derivable candidate clock change.
fn run_two_gc_scans(repo: &Path) {
    for entry in walkdir::WalkDir::new(repo.join(".libra/objects")) {
        let entry = entry.expect("enumerate fixture objects");
        if entry.file_type().is_file() {
            std::fs::File::open(entry.path())
                .expect("open fixture object")
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
                .expect("age fixture object");
        }
    }
    let args = ["maintenance", "run", "--task", "gc"];
    let first = run_libra_command(&args, repo);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let ledger_path = repo.join(".libra/gc-prune-candidates.json");
    let mut ledger: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&ledger_path).expect("first GC created candidate ledger"),
    )
    .expect("candidate ledger");
    for first_seen in ledger.as_object_mut().expect("candidate map").values_mut() {
        *first_seen = json!(1);
    }
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&ledger).expect("encode aged ledger"),
    )
    .expect("age quarantine candidates");
    let second = run_libra_command(&args, repo);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[tokio::test]
async fn agent_clean_gc_keeps_reachable_reasoning_artifact() {
    let repo = tempfile::tempdir().expect("repo");
    init_repo_via_cli(repo.path());
    let (id, sha, artifact_oid) = seed_reasoning_artifact(repo.path()).await;
    let (temporary_id, previous_head) = seed_temporary_gc_checkpoint(repo.path()).await;
    let orphan = write_git_object(
        &repo.path().join(".libra"),
        "blob",
        b"unreachable GC control",
    )
    .expect("seed unreachable GC control");
    run_two_gc_scans(repo.path());
    assert!(
        !artifact_object_path(repo.path(), &orphan.to_string()).exists(),
        "GC must collect the control object"
    );
    assert!(
        artifact_object_path(repo.path(), &artifact_oid.to_string()).exists(),
        "reachable artifact survives real GC"
    );
    let out = run_libra_command(&["agent", "clean", "--gc"], repo.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let post_prune_orphan = write_git_object(
        &repo.path().join(".libra"),
        "blob",
        b"post-retention unreachable GC control",
    )
    .expect("seed post-retention GC control");
    run_two_gc_scans(repo.path());
    assert!(
        !artifact_object_path(repo.path(), &post_prune_orphan.to_string()).exists(),
        "real GC after retention rewrite must collect its control object"
    );
    // RG-06 precedes RG-03's authorized export option. Read the actual
    // surviving checkpoint manifest so this GC gate verifies its own
    // reachability contract without requiring a later card's reader API.
    let conn = connect_repo_db(repo.path()).await;
    assert!(
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint WHERE checkpoint_id = ?",
            [temporary_id.into()],
        ))
        .await
        .expect("temporary checkpoint removal")
        .is_none(),
        "clean must actually prune the temporary checkpoint"
    );
    let ref_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT \"commit\" FROM reference WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
            [TRACES_BRANCH.into()],
        ))
        .await
        .expect("post-retention traces reference")
        .expect("traces reference survives");
    assert_ne!(
        ref_row
            .try_get_by::<String, _>("commit")
            .expect("rewritten traces head"),
        previous_head.to_string(),
        "clean must actually rewrite traces before post-retention object GC"
    );
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT tree_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
            [id.clone().into()],
        ))
        .await
        .expect("post-GC checkpoint tree")
        .expect("checkpoint survives GC");
    let root_oid: ObjectHash = row
        .try_get_by::<String, _>("tree_oid")
        .expect("post-GC tree oid")
        .parse()
        .expect("valid post-GC tree oid");
    let storage = repo.path().join(".libra");
    let mut tree = load_fixture_tree(&storage, root_oid);
    for name in ["checkpoint", &id[..2], &id[2..]] {
        let subtree = tree
            .iter()
            .find(|entry| entry.name == name)
            .expect("post-GC checkpoint subtree")
            .id;
        tree = load_fixture_tree(&storage, subtree);
    }
    let manifest_oid = tree
        .iter()
        .find(|entry| entry.name == "manifest.json")
        .expect("post-GC checkpoint manifest")
        .id;
    let manifest: serde_json::Value = serde_json::from_slice(
        &read_git_object(&storage, &manifest_oid).expect("post-GC manifest bytes"),
    )
    .expect("post-GC manifest JSON");
    let artifact = &manifest["reasoning_artifacts"][0];
    assert_eq!(artifact["sha256"], sha);
    assert_eq!(artifact["oid"], artifact_oid.to_string());
    let declared_oid: ObjectHash = artifact["oid"]
        .as_str()
        .expect("manifest artifact oid")
        .parse()
        .expect("valid manifest artifact oid");
    assert_eq!(
        read_git_object(&storage, &declared_oid).expect("artifact survives GC"),
        ARTIFACT_CANARY
    );
    conn.close().await.expect("close post-GC database");
}

async fn seed_reasoning_artifact(repo: &Path) -> (String, String, ObjectHash) {
    let id = seed_checkpoint_with_secret(repo).await;
    let storage = repo.join(".libra");
    let conn = connect_repo_db(repo).await;
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT tree_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
            [id.clone().into()],
        ))
        .await
        .expect("checkpoint tree")
        .expect("checkpoint row");
    let root_oid: ObjectHash = row
        .try_get_by::<String, _>("tree_oid")
        .expect("tree oid")
        .parse()
        .expect("parse tree oid");
    let mut root = load_fixture_tree(&storage, root_oid);
    let checkpoint_entry = root
        .iter_mut()
        .find(|item| item.name == "checkpoint")
        .expect("checkpoint subtree");
    let mut checkpoints = load_fixture_tree(&storage, checkpoint_entry.id);
    let prefix_entry = checkpoints
        .iter_mut()
        .find(|item| item.name == id[..2])
        .expect("prefix subtree");
    let mut prefix = load_fixture_tree(&storage, prefix_entry.id);
    let inner_entry = prefix
        .iter_mut()
        .find(|item| item.name == id[2..])
        .expect("checkpoint leaf");
    let mut inner = load_fixture_tree(&storage, inner_entry.id);
    let sha = format!("{:x}", Sha256::digest(ARTIFACT_CANARY));
    let oid =
        write_git_object(&storage, "blob", ARTIFACT_CANARY).expect("write synthetic ciphertext");
    let leaf = TreeItem::new(TreeItemMode::Blob, oid, sha.clone());
    let encrypted = write_fixture_tree(&storage, vec![leaf]);
    let reasoning = write_fixture_tree(
        &storage,
        vec![TreeItem::new(
            TreeItemMode::Tree,
            encrypted,
            "encrypted".to_string(),
        )],
    );
    inner.push(TreeItem::new(
        TreeItemMode::Tree,
        reasoning,
        "reasoning".to_string(),
    ));
    let manifest_entry = inner
        .iter_mut()
        .find(|item| item.name == "manifest.json")
        .expect("manifest");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &read_git_object(&storage, &manifest_entry.id).expect("manifest bytes"),
    )
    .expect("manifest JSON");
    manifest["reasoning_artifacts"] = json!([{
        "path": format!("reasoning/encrypted/{sha}"), "oid": oid.to_string(), "sha256": sha,
        "locator": "claude_code:msg=0/part=0/metadata=signature", "provider": "claude_code",
        "source_kind": "signature", "availability": "encrypted_unavailable", "decrypt_capability": "none", "byte_len": ARTIFACT_CANARY.len()
    }]);
    manifest_entry.id = write_git_object(
        &storage,
        "blob",
        &serde_json::to_vec(&manifest).expect("encode artifact manifest"),
    )
    .expect("artifact manifest blob");
    inner_entry.id = write_fixture_tree(&storage, inner);
    prefix_entry.id = write_fixture_tree(&storage, prefix);
    checkpoint_entry.id = write_fixture_tree(&storage, checkpoints);
    let tree_oid = write_fixture_tree(&storage, root);
    let signature = |kind| Signature::new(kind, "Libra".to_string(), "traces@libra".to_string());
    let message = format!(
        "traces: committed checkpoint {id}\n\nLibra-Session: sess-x\nLibra-Agent: claude_code\nLibra-Checkpoint-ID: {id}\nLibra-Scope: committed\n"
    );
    let commit = Commit::new(
        signature(SignatureType::Author),
        signature(SignatureType::Committer),
        tree_oid,
        vec![],
        &message,
    );
    let commit_oid = write_git_object(
        &storage,
        "commit",
        &commit.to_data().expect("encode artifact fixture commit"),
    )
    .expect("write artifact fixture commit");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_checkpoint SET tree_oid = ?, traces_commit = ?, created_at = ? WHERE checkpoint_id = ?",
        [tree_oid.to_string().into(), commit_oid.to_string().into(), chrono::Utc::now().timestamp_millis().into(), id.clone().into()],
    ))
    .await
    .expect("install reader fixture");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE reference SET \"commit\" = ? WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
        [commit_oid.to_string().into(), TRACES_BRANCH.into()],
    ))
    .await
    .expect("make artifact ref-reachable");
    conn.close().await.expect("close artifact fixture database");
    (id, sha, oid)
}
