//! Synthetic lifecycle evidence only: no provider execution or native-source claim.
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

pub const SYNTHETIC_BODY: &[u8] = br"synthetic-opaque-reasoning-byte-exact-\u0041";

pub async fn seed(repo: &Path) -> (String, String, ObjectHash) {
    let id = seed_checkpoint(repo).await;
    attach(repo, id, "sess-x", SYNTHETIC_BODY, None).await
}

#[allow(dead_code)] // Shared fixture: only the local doctor target needs this case.
pub async fn seed_with_bad_digest(repo: &Path) -> (String, String, ObjectHash) {
    let id = seed_checkpoint(repo).await;
    attach(repo, id, "sess-x", SYNTHETIC_BODY, Some(&"0".repeat(64))).await
}

pub async fn erase(repo: &Path) {
    let conn = connect_catalog_db(repo).await;
    let storage = repo.join(".libra");
    let manager = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init_local_existing(storage.join("objects"))),
        storage,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    manager
        .erase_session_local("sess-x")
        .await
        .expect("erase the actual synthetic fixture session");
    conn.close().await.expect("close erasure database");
}

#[allow(dead_code)] // Safe metadata only for a failed live fixture, never object bodies.
pub async fn index_diagnostics(repo: &Path) -> serde_json::Value {
    let conn = connect_repo_db(repo).await;
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT o_id, o_type, o_size, is_synced, repo_id FROM object_index ORDER BY o_id"
                .to_string(),
        ))
        .await
        .unwrap();
    let indexes = rows
        .iter()
        .map(|row| {
            json!({
                "oid":row.try_get_by::<String,_>("o_id").unwrap(),
                "type":row.try_get_by::<String,_>("o_type").unwrap(),
                "size":row.try_get_by::<i64,_>("o_size").unwrap(),
                "synced":row.try_get_by::<i64,_>("is_synced").unwrap(),
                "repo_id":row.try_get_by::<String,_>("repo_id").unwrap(),
            })
        })
        .collect::<Vec<_>>();
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT traces_commit, tree_oid FROM agent_checkpoint LIMIT 1".to_string(),
        ))
        .await
        .unwrap()
        .unwrap();
    let mut required = std::collections::BTreeSet::new();
    required.insert(row.try_get_by::<String, _>("traces_commit").unwrap());
    let mut pending = vec![row.try_get_by::<String, _>("tree_oid").unwrap()];
    while let Some(oid) = pending.pop() {
        if !required.insert(oid.clone()) {
            continue;
        }
        let hash =
            ObjectHash::from_hex_for_kind(git_internal::hash::get_hash_kind(), &oid).unwrap();
        let tree = load_fixture_tree(&repo.join(".libra"), hash);
        for item in tree {
            if item.mode == TreeItemMode::Tree {
                pending.push(item.id.to_string());
            } else {
                required.insert(item.id.to_string());
            }
        }
    }
    conn.close().await.unwrap();
    json!({"required":required,"indexes":indexes})
}

async fn attach(
    repo: &Path,
    id: String,
    session_id: &str,
    body: &[u8],
    digest_override: Option<&str>,
) -> (String, String, ObjectHash) {
    let storage = repo.join(".libra");
    let conn = connect_catalog_db(repo).await;
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
    let sha = digest_override
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:x}", Sha256::digest(body)));
    let oid = write_git_object(&storage, "blob", body).expect("write synthetic ciphertext");
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
        "source_kind": "signature", "availability": "encrypted_unavailable", "decrypt_capability": "none", "byte_len": body.len()
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
        "traces: committed checkpoint {id}\n\nLibra-Session: {session_id}\nLibra-Agent: claude_code\nLibra-Checkpoint-ID: {id}\nLibra-Scope: committed\n"
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
    index_fixture_objects(&conn, &storage).await;
    conn.close().await.expect("close artifact fixture database");
    (id, sha, oid)
}

async fn index_fixture_objects(conn: &DatabaseConnection, storage: &Path) {
    let repo_id: String = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1"
                .to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by("value")
        .unwrap();
    for prefix in std::fs::read_dir(storage.join("objects"))
        .unwrap()
        .flatten()
    {
        let name = prefix.file_name().to_string_lossy().into_owned();
        if name.len() != 2 || !prefix.path().is_dir() {
            continue;
        }
        for file in std::fs::read_dir(prefix.path()).unwrap().flatten() {
            let oid = format!("{name}{}", file.file_name().to_string_lossy());
            let Ok(hash) = oid.parse::<ObjectHash>() else {
                continue;
            };
            let existing = conn
                .query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT id FROM object_index WHERE repo_id = ? AND o_id = ?",
                    [repo_id.clone().into(), oid.clone().into()],
                ))
                .await
                .unwrap();
            if existing.is_some() {
                continue;
            }
            use std::io::Read;
            let mut decoded = Vec::new();
            flate2::read::ZlibDecoder::new(std::fs::File::open(file.path()).unwrap())
                .take(32 * 1024 * 1024 + 65)
                .read_to_end(&mut decoded)
                .unwrap();
            let header_end = decoded.iter().position(|byte| *byte == 0).unwrap();
            assert!(header_end <= 64);
            let header = std::str::from_utf8(&decoded[..header_end]).unwrap();
            let (kind, size) = header.split_once(' ').unwrap();
            let bytes = &decoded[header_end + 1..];
            assert_eq!(bytes.len() as u64, size.parse::<u64>().unwrap());
            assert!(bytes.len() <= 32 * 1024 * 1024);
            assert_eq!(
                git_internal::hash::ObjectHash::from_type_and_data_for_kind(
                    hash.kind(),
                    git_internal::internal::object::types::ObjectType::from_string(kind).unwrap(),
                    bytes
                )
                .unwrap(),
                hash
            );
            conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) VALUES (?, ?, ?, ?, 1, 0) ON CONFLICT(repo_id, o_id) DO NOTHING",
                [oid.into(), kind.into(), (bytes.len() as i64).into(), repo_id.clone().into()])).await.unwrap();
        }
    }
}

async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts).await.expect("connect repo db")
}

/// Catalog tables live in `.libra/private/capture.db` once the capture store
/// has opened (it copies rows seeded into `libra.db` before that).
async fn connect_catalog_db(repo: &Path) -> DatabaseConnection {
    let capture = repo.join(".libra").join("private").join("capture.db");
    if !capture.is_file() {
        return connect_repo_db(repo).await;
    }
    let mut opts = ConnectOptions::new(format!("sqlite://{}", capture.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts)
        .await
        .expect("connect capture database")
}

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

async fn seed_checkpoint(repo: &Path) -> String {
    let conn = connect_catalog_db(repo).await;
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
    let storage = Arc::new(ClientStorage::init_local_existing(
        repo_path.join("objects"),
    ));
    let history =
        HistoryManager::new_with_ref(storage, repo_path, Arc::new(conn.clone()), TRACES_BRANCH);
    let redactor = Redactor::new_default();
    let (redacted, _) = redactor.redact(b"ordinary synthetic lifecycle fixture transcript");
    let (meta_redacted, _) = redactor.redact(br#"{"checkpoint_id":"x"}"#);
    let (events_redacted, _) = redactor.redact(b"{}\n");
    let (report_redacted, _) = redactor.redact(b"{}");
    let checkpoint_id = "aabbccdd-eeff-0011-2233-445566778899".to_string();
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
    // The writer marker is gone. Drain the object-index queue before erase
    // so a leftover repair marker cannot outlive the checkpoint.
    assert!(
        ClientStorage::wait_for_background_tasks_until(
            std::time::Instant::now() + Duration::from_secs(10),
        )
        .await,
        "seeded checkpoint object indexing did not finish within 10 seconds"
    );
    let marker_dir = repo.join(".libra").join("object-index-repair");
    let marker_names = match std::fs::read_dir(&marker_dir) {
        Ok(entries) => entries
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap_or_else(|error| {
                panic!(
                    "cannot enumerate object-index repair markers at {}: {error}",
                    marker_dir.display()
                )
            })
            .into_iter()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!(
            "cannot inspect object-index repair markers at {}: {error}",
            marker_dir.display()
        ),
    };
    assert!(
        marker_names.is_empty(),
        "seeded checkpoint left object-index repair markers: {marker_names:?}"
    );
    if let Ok(Some(entry)) =
        libra::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid").await
        && !entry.encrypted
    {
        eprintln!(
            "reasoning fixture index drain complete repo_id={}",
            entry.value
        );
    }
    conn.close().await.expect("close seed conn");
    checkpoint_id
}
