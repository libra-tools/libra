use super::*;
use crate::internal::ai::history::{
    ReasoningArtifactCheckpointProof, ReasoningArtifactCheckpointSpec,
    ReasoningArtifactObjectProof, checkpoint_reasoning_artifact_proofs,
};

/// The exact trees whose role/digest/size checks succeeded before publication.
/// This evidence must also match the final fenced catalog and object index.
struct ReasoningArtifactPublicationProof {
    checkpoints: Vec<ReasoningArtifactCheckpointProof>,
    objects: HashMap<String, ReasoningArtifactObjectProof>,
}

impl ReasoningArtifactPublicationProof {
    async fn establish(
        storage: &std::path::Path,
        checkpoints: &[AgentCheckpointV2Row],
        deadline: std::time::Instant,
    ) -> CloudResult<Self> {
        let specs = checkpoints
            .iter()
            .map(|row| ReasoningArtifactCheckpointSpec {
                checkpoint_id: row.checkpoint_id.clone(),
                tree_oid: row.tree_oid.clone(),
            })
            .collect::<Vec<_>>();
        let evidence = checkpoint_reasoning_artifact_proofs(storage, &specs, deadline)
            .await
            .map_err(|_| artifact_integrity_failure())?;
        let mut objects = HashMap::new();
        for proof in &evidence {
            for artifact in proof
                .artifacts
                .as_ref()
                .ok_or_else(artifact_integrity_failure)?
            {
                if let Some(previous) = objects.insert(artifact.oid.clone(), artifact.clone())
                    && (previous.sha256 != artifact.sha256
                        || previous.byte_len != artifact.byte_len)
                {
                    return Err(artifact_integrity_failure());
                }
            }
        }
        let result = Self {
            checkpoints: evidence,
            objects,
        };
        result.validate_catalog(checkpoints)?;
        Ok(result)
    }

    fn validate_catalog(&self, checkpoints: &[AgentCheckpointV2Row]) -> CloudResult<()> {
        let trees = checkpoints
            .iter()
            .map(|row| (row.checkpoint_id.as_str(), row.tree_oid.as_str()))
            .collect::<HashMap<_, _>>();
        if trees.len() != checkpoints.len()
            || trees.len() != self.checkpoints.len()
            || self.checkpoints.iter().any(|proof| {
                proof.artifacts.is_none()
                    || trees.get(proof.checkpoint_id.as_str()).copied()
                        != Some(proof.tree_oid.as_str())
            })
        {
            return Err(artifact_integrity_failure());
        }
        Ok(())
    }

    fn validate_indexes(&self, indexes: &[ObjectIndexRow]) -> CloudResult<()> {
        let rows = indexes
            .iter()
            .map(|row| (row.o_id.as_str(), row))
            .collect::<HashMap<_, _>>();
        if self.objects.values().any(|proof| {
            !rows.get(proof.oid.as_str()).is_some_and(|row| {
                row.o_type == "blob"
                    && row.is_synced == 1
                    && u64::try_from(row.o_size).ok() == Some(proof.byte_len)
            })
        }) {
            return Err(artifact_integrity_failure());
        }
        Ok(())
    }
}

fn artifact_integrity_failure() -> CloudError {
    CloudError::PartialTransfer(
        "reasoning artifact role, digest, size, or object index is inconsistent; run `libra agent doctor` and repair the checkpoint before retrying cloud sync".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use git_internal::internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        types::ObjectType,
    };
    use sea_orm::{ConnectionTrait, Statement};

    use super::*;
    use crate::internal::{
        config::ConfigKv,
        metadata::{MetadataKv, MetadataScope, MetadataValueType},
    };

    #[derive(Clone, Copy)]
    enum ArtifactFault {
        None,
        Missing,
        Size,
        Digest,
    }

    struct FaultyArtifactStorage {
        remote: RemoteStorage,
        artifact: ObjectHash,
        fault: ArtifactFault,
        written: AtomicBool,
        artifact_writes: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Storage for FaultyArtifactStorage {
        async fn get(
            &self,
            hash: &ObjectHash,
        ) -> Result<(Vec<u8>, ObjectType), git_internal::errors::GitError> {
            if *hash == self.artifact && !matches!(self.fault, ArtifactFault::None) {
                if !self.written.load(Ordering::SeqCst)
                    || matches!(self.fault, ArtifactFault::Missing)
                {
                    return Err(git_internal::errors::GitError::ObjectNotFound(
                        "fixture unavailable".into(),
                    ));
                }
                let mut body =
                    crate::internal::ai::checkpoint_reader::test_helpers::RG03_OPAQUE_BODY.to_vec();
                match self.fault {
                    ArtifactFault::Size => {
                        body.pop();
                    }
                    ArtifactFault::Digest => {
                        body[0] ^= 1;
                    }
                    _ => {}
                }
                return Ok((body, ObjectType::Blob));
            }
            self.remote.get(hash).await
        }

        async fn get_typed_bounded(
            &self,
            hash: &ObjectHash,
            cap: u64,
        ) -> Result<(Vec<u8>, ObjectType), git_internal::errors::GitError> {
            if *hash == self.artifact && !matches!(self.fault, ArtifactFault::None) {
                let (bytes, kind) = self.get(hash).await?;
                if bytes.len() as u64 > cap {
                    return Err(git_internal::errors::GitError::InvalidObjectInfo(
                        "fixture exceeds bound".into(),
                    ));
                }
                return Ok((bytes, kind));
            }
            self.remote.get_typed_bounded(hash, cap).await
        }

        async fn put(
            &self,
            hash: &ObjectHash,
            bytes: &[u8],
            kind: ObjectType,
        ) -> Result<String, git_internal::errors::GitError> {
            if *hash == self.artifact {
                self.artifact_writes.fetch_add(1, Ordering::SeqCst);
                self.written.store(true, Ordering::SeqCst);
            }
            self.remote.put(hash, bytes, kind).await
        }

        async fn exist(&self, hash: &ObjectHash) -> bool {
            self.remote.exist(hash).await
        }
        async fn search(&self, prefix: &str) -> Vec<ObjectHash> {
            self.remote.search(prefix).await
        }
    }

    async fn artifact_sync_fixture() -> (tempfile::TempDir, sea_orm::DatabaseConnection, ObjectHash)
    {
        use crate::internal::ai::checkpoint_reader::{
            read_tree_object, subtree, test_helpers::*, tree_entry,
        };

        let repo = tempfile::tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        let storage = repo.path().join(".libra");
        let conn = crate::internal::db::get_db_conn_instance().await;
        ConfigKv::set_with_conn(&conn, "libra.repoid", "rg05-fixture", false)
            .await
            .unwrap();
        let (root, artifact) = rg03_read_fixture(&storage, "valid", false);
        let tree = read_tree_object(&storage, &root).unwrap();
        let cp = subtree(&storage, &tree, "checkpoint").unwrap();
        let prefix = subtree(&storage, &cp, &RG03_CHECKPOINT_ID[..2]).unwrap();
        let inner = subtree(&storage, &prefix, &RG03_CHECKPOINT_ID[2..]).unwrap();
        let metadata = tree_entry(&inner, "metadata.json").unwrap().id;
        let signature = |kind| Signature::new(kind, "Libra".into(), "traces@libra".into());
        let commit = Commit::new(
            signature(SignatureType::Author),
            signature(SignatureType::Committer),
            root.parse().unwrap(),
            vec![],
            &format!(
                "traces: committed checkpoint {RG03_CHECKPOINT_ID}\n\nLibra-Session: rg05-session\nLibra-Agent: claude_code\nLibra-Checkpoint-ID: {RG03_CHECKPOINT_ID}\nLibra-Scope: committed\n"
            ),
        );
        let commit =
            crate::utils::object::write_git_object(&storage, "commit", &commit.to_data().unwrap())
                .unwrap();
        for (sql, values) in [
            (
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, started_at, last_event_at, metadata_json, redaction_report) VALUES ('rg05-session', 'claude_code', 'rg05-provider', 'stopped', '/tmp/rg05-fixture', 1, 2, '{}', '{}')",
                vec![],
            ),
            (
                "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES (?, 'rg05-session', 'committed', ?, ?, ?, 3)",
                vec![
                    RG03_CHECKPOINT_ID.into(),
                    root.into(),
                    metadata.to_string().into(),
                    commit.to_string().into(),
                ],
            ),
            (
                "UPDATE reference SET `commit` = ? WHERE name = ? AND remote IS NULL",
                vec![
                    commit.to_string().into(),
                    crate::internal::branch::TRACES_BRANCH.into(),
                ],
            ),
        ] {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                sql,
                values,
            ))
            .await
            .unwrap();
        }
        // Index the complete actual fixture object graph, not fabricated OIDs.
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
                let Ok(hash) = crate::internal::object_format::parse_repo_oid(&oid) else {
                    continue;
                };
                let (kind, bytes) = crate::utils::object::read_git_object_bounded_validated(
                    &storage,
                    &hash,
                    4 * 1024 * 1024,
                )
                .unwrap();
                conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                    "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) VALUES (?, ?, ?, 'rg05-fixture', 1, 1)",
                    [oid.into(), kind.into(), (bytes.len() as i64).into()])).await.unwrap();
            }
        }
        (repo, conn, artifact)
    }

    async fn assert_artifact_sync_boundary(fault: ArtifactFault) {
        let _env = crate::utils::test::ConfigDbFixture::new().unwrap();
        let (repo, conn, artifact) = artifact_sync_fixture().await;
        let _repo = crate::utils::test::ChangeDirGuard::new(repo.path());
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let client = mock.client();
        client.ensure_object_index_table().await.unwrap();
        let remote = FaultyArtifactStorage {
            remote: RemoteStorage::new(Arc::new(object_store::memory::InMemory::new())),
            artifact,
            fault,
            written: AtomicBool::new(false),
            artifact_writes: AtomicUsize::new(0),
        };
        let frozen = freeze_agent_capture(&conn, None).await;
        let result = sync_agent_capture_tables_inner(
            &conn,
            &client,
            &remote,
            "rg05-fixture",
            &SilentCloudSyncProgress,
            &frozen,
        )
        .await;
        if matches!(fault, ArtifactFault::None) {
            assert!(result.is_ok(), "normal first upload: {result:?}");
            assert_eq!(
                client
                    .get_agent_capture_generation("rg05-fixture")
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                "complete"
            );
        } else {
            assert!(result.is_err(), "post-upload artifact corruption must fail");
            assert!(
                client
                    .get_agent_capture_generation("rg05-fixture")
                    .await
                    .unwrap()
                    .is_none(),
                "failed artifact cannot begin/complete a generation"
            );
        }
        assert_eq!(
            remote.artifact_writes.load(Ordering::SeqCst),
            1,
            "first missing remote object was actually uploaded before readback"
        );
        conn.close().await.unwrap();
    }

    /// plan-20261116 FIX-CQ-02: once the catalog moved to `capture.db`,
    /// `libra.db` has no `agent_*` tables. Cloud sync still freezes the
    /// catalog, reading it from `capture.db`, and reads the traces head from
    /// `libra.db`.
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn split_catalog_is_frozen_from_the_capture_database() {
        let _env = crate::utils::test::ConfigDbFixture::new().unwrap();
        let repo = tempfile::tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        let storage = repo.path().join(".libra");
        let capture = crate::internal::ai::capture::store::open(
            &storage,
            crate::internal::ai::capture::store::COMMAND_BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        capture
            .execute_unprepared(
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, started_at, last_event_at, metadata_json, redaction_report) VALUES ('split-session', 'claude_code', 'split-provider', 'stopped', '/tmp/split-fixture', 1, 2, '{}', '{}')",
            )
            .await
            .unwrap();
        let repository = crate::internal::db::get_db_conn_instance().await;
        assert!(
            !crate::internal::db::connection_has_table(&repository, "agent_session")
                .await
                .unwrap(),
            "the fixture must be a split repository"
        );
        let catalog = local_capture_catalog(&repository).await.unwrap();
        match freeze_agent_capture(&catalog, None).await {
            FrozenAgentCapture::Catalog(frozen) => {
                assert_eq!(frozen.sessions.len(), 1);
                assert_eq!(frozen.sessions[0].session_id, "split-session");
            }
            _ => panic!("the split catalog must be frozen from capture.db"),
        }
    }

    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn remote_missing_artifact_fails_sync() {
        assert_artifact_sync_boundary(ArtifactFault::Missing).await;
    }

    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn remote_size_mismatch_fails_sync() {
        assert_artifact_sync_boundary(ArtifactFault::Size).await;
    }

    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn remote_digest_mismatch_fails_sync() {
        assert_artifact_sync_boundary(ArtifactFault::Digest).await;
    }

    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn first_missing_artifact_is_uploaded_and_completed() {
        assert_artifact_sync_boundary(ArtifactFault::None).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn artifact_lifecycle_helper_proves_roles_without_returning_ciphertext() {
        use std::os::unix::ffi::OsStrExt;

        use base64::Engine;

        use crate::internal::ai::checkpoint_reader::test_helpers::*;
        for kind in [
            git_internal::hash::HashKind::Sha1,
            git_internal::hash::HashKind::Sha256,
            git_internal::hash::HashKind::Blake3,
        ] {
            let _kind = git_internal::hash::set_hash_kind_for_test(kind);
            for shape in [
                "valid",
                "tree_alias",
                "manifest_alias",
                "metadata_body_alias",
                "unknown_reasoning",
            ] {
                let root = tempfile::tempdir().unwrap();
                let (tree, _) = rg03_read_fixture(root.path(), shape, false);
                let frame = serde_json::to_vec(&serde_json::json!({
                    "repo_path_base64":base64::engine::general_purpose::STANDARD.encode(root.path().as_os_str().as_bytes()),
                    "operation":{"kind":"verify_reasoning_artifacts", "object_format":crate::internal::object_format::as_str(kind),
                        "checkpoints":[{"checkpoint_id":RG03_CHECKPOINT_ID,"tree_oid":tree}]}
                })).unwrap();
                let bytes =
                    crate::internal::ai::history::run_checkpoint_object_io_helper(&frame).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes)
                        .contains(std::str::from_utf8(RG03_OPAQUE_BODY).unwrap())
                );
                let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let artifact = &result["checkpoints"][0]["artifacts"];
                if shape == "valid" {
                    assert_eq!(artifact[0]["byte_len"], RG03_OPAQUE_BODY.len());
                    assert_eq!(
                        artifact[0]["sha256"],
                        hex::encode(Sha256::digest(RG03_OPAQUE_BODY))
                    );
                } else {
                    assert!(artifact.is_null(), "{shape}: {result}");
                }
            }
        }
        let root = tempfile::tempdir().unwrap();
        let expired =
            checkpoint_reasoning_artifact_proofs(root.path(), &[], std::time::Instant::now()).await;
        assert!(
            expired.is_err(),
            "an expired absolute budget must refuse before helper dispatch"
        );
    }

    fn snapshot_wire_bytes(snapshot: &AgentCaptureSnapshot) -> Vec<u8> {
        let mut required_oids = snapshot.required_oids.iter().collect::<Vec<_>>();
        required_oids.sort();
        serde_json::to_vec(&serde_json::json!({
            "sessions": snapshot.sessions, "checkpoints": snapshot.checkpoints,
            "claims": snapshot.claims, "revisions": snapshot.revisions,
            "links": snapshot.links, "prune_tombstones": snapshot.prune_tombstones,
            "import_tombstones": snapshot.import_tombstones,
            "required_oids": required_oids, "traces_head": snapshot.traces_head,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn private_capture_scopes_do_not_enter_closed_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("repository.db");
        let conn = crate::internal::db::create_database(path.to_str().unwrap())
            .await
            .unwrap();
        ConfigKv::set_with_conn(&conn, "libra.repoid", "cloud-private-repo", false)
            .await
            .unwrap();
        let existing_pk = "claude__native-session";
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
              state, working_dir, started_at, last_event_at, metadata_json,
              redaction_report, sync_revision, repo_id, worktree_id, scope_state)
             VALUES (?, 'claude_code', 'native-session', 'active', '/existing/catalog/path',
               1, 1, '{}', '{}', 1, 'cloud-private-repo', '', 'scoped')",
            [existing_pk.into()],
        ))
        .await
        .unwrap();
        let before = load_agent_capture_catalog_snapshot(&conn, "cloud-private-repo", true)
            .await
            .unwrap();
        let before_bytes = snapshot_wire_bytes(&before);
        let alias = uuid::Uuid::new_v4().to_string();
        let mac = format!("pending-alias/hmac-v1/{}", "b".repeat(64));
        let body = serde_json::json!({"alias": alias, "mac": mac,
            "session_id": existing_pk, "private_canary": "private-only-association"})
        .to_string();
        for (scope, value) in [
            (MetadataScope::AgentCaptureSessionAlias, body.as_str()),
            (MetadataScope::AgentCapturePending, "private-only-header"),
            (
                MetadataScope::AgentCaptureQuarantine,
                "private-only-quarantine",
            ),
            (
                MetadataScope::AgentCapturePendingChunk,
                "private-only-chunk",
            ),
        ] {
            MetadataKv::set_with_conn(
                &conn,
                scope,
                "cloud-private-repo",
                &alias,
                value,
                MetadataValueType::Text,
            )
            .await
            .unwrap();
        }
        for (key, canary) in [
            ("identity-v1", "private-only-identity-v1"),
            ("identity-v2", "private-only-identity-v2"),
        ] {
            MetadataKv::set_with_conn(
                &conn,
                MetadataScope::AgentReasoningIdentity,
                existing_pk,
                key,
                canary,
                MetadataValueType::Text,
            )
            .await
            .unwrap();
            assert!(
                MetadataKv::get_with_conn(
                    &conn,
                    MetadataScope::AgentReasoningIdentity,
                    existing_pk,
                    key,
                )
                .await
                .unwrap()
                .is_some(),
                "both private identity keys must exist before the cloud snapshot"
            );
        }
        let after = load_agent_capture_catalog_snapshot(&conn, "cloud-private-repo", true)
            .await
            .unwrap();
        assert_eq!(before, after);
        let after_bytes = snapshot_wire_bytes(&after);
        assert_eq!(before_bytes, after_bytes);
        let wire = String::from_utf8(after_bytes).unwrap();
        assert!(
            wire.contains(existing_pk),
            "existing catalog wire is intentionally unchanged"
        );
        for canary in [
            &alias,
            &mac,
            "private-only-association",
            "private-only-header",
            "private-only-quarantine",
            "private-only-chunk",
            "private-only-identity-v1",
            "private-only-identity-v2",
            "agent_capture_session_alias",
            "agent_capture_pending",
            "agent_capture_quarantine",
            "agent_capture_pending_chunk",
            "agent_reasoning_identity",
            "identity-v1",
            "identity-v2",
        ] {
            assert!(
                !wire.contains(canary),
                "private canary entered closed cloud snapshot"
            );
        }
    }
}

const AGENT_CAPTURE_LOCAL_PAGE_SIZE: usize = 256;
pub(super) const AGENT_CAPTURE_MAX_ROWS_PER_TABLE: usize = 100_000;
pub(super) const AGENT_CAPTURE_RESTORE_MAX_ROWS: usize = 100_000;
const AGENT_CAPTURE_D1_BATCH_SIZE: usize = 128;
const AGENT_CAPTURE_OBJECT_VERIFY_CONCURRENCY: usize = 32;
pub(super) const AGENT_CAPTURE_CLOUD_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(120);

/// Map local catalog/SQLite failures to a fixed diagnostic. Database drivers
/// can include SQL fragments or decoded values in their Display output, and
/// a restored catalog may contain remote-controlled capture identifiers.
/// Keep that material out of CloudError and tracing alike.
/// The capture catalog paired with a repository connection.
///
/// After the catalog moved to `.libra/private/capture.db`, `libra.db` no
/// longer holds the `agent_*` tables. A connection that still holds
/// `agent_session` (a combined database, such as a test fixture or a
/// repository this release has not opened yet) is its own catalog. Otherwise
/// the repository's `capture.db` is opened through the capture store, which
/// creates it (and moves a pre-split catalog) when it is missing. The two
/// files are never attached.
pub(super) async fn local_capture_catalog(
    conn: &sea_orm::DatabaseConnection,
) -> CloudResult<sea_orm::DatabaseConnection> {
    if crate::internal::db::connection_has_table(conn, "agent_session")
        .await
        .map_err(|error| local_agent_capture_failure("probe local capture catalog", error))?
    {
        return Ok(conn.clone());
    }
    let storage = crate::internal::db::connection_storage_root(conn)
        .await
        .map_err(|error| local_agent_capture_failure("locate local capture catalog", error))?;
    crate::internal::ai::capture::store::open(
        &storage,
        crate::internal::ai::capture::store::COMMAND_BUSY_TIMEOUT,
    )
    .await
    .map_err(|error| local_agent_capture_failure("open local capture catalog", error))
}

pub(super) fn local_agent_capture_failure(
    operation: &'static str,
    _error: impl std::fmt::Display,
) -> CloudError {
    tracing::warn!(
        agent_capture_operation = operation,
        "local agent-capture catalog operation failed"
    );
    CloudError::Generic(format!(
        "local agent-capture catalog {operation} failed; run `libra agent doctor --repair` and retry cloud sync"
    ))
}

/// Restore-side counterpart of [`local_agent_capture_failure`]: the same
/// fixed diagnostic, pointing back at `libra cloud restore`.
pub(super) fn local_agent_capture_restore_failure(
    operation: &'static str,
    _error: impl std::fmt::Display,
) -> CloudError {
    tracing::warn!(
        agent_capture_operation = operation,
        "local agent-capture catalog operation failed during cloud restore"
    );
    CloudError::Generic(format!(
        "local agent-capture catalog {operation} failed during cloud restore; run `libra agent doctor --repair` and retry `libra cloud restore`"
    ))
}

pub(super) fn agent_capture_batches<T>(rows: &[T]) -> std::slice::Chunks<'_, T> {
    rows.chunks(AGENT_CAPTURE_D1_BATCH_SIZE)
}

pub(super) fn agent_capture_object_verification_batches<T>(
    rows: &[T],
) -> std::slice::Chunks<'_, T> {
    rows.chunks(AGENT_CAPTURE_OBJECT_VERIFY_CONCURRENCY)
}

pub(super) async fn load_local_agent_capture_cloud_base(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
) -> CloudResult<Option<i64>> {
    use sea_orm::Statement;

    let backend = db_conn.get_database_backend();
    let table_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_capture_cloud_base' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| local_agent_capture_failure("probe local cloud base", error))?
        .is_some();
    if !table_present {
        return Ok(None);
    }
    db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT remote_generation FROM agent_capture_cloud_base WHERE repo_id = ?",
            [repo_id.into()],
        ))
        .await
        .map_err(|error| local_agent_capture_failure("read local cloud base", error))?
        .map(|row| {
            row.try_get_by("remote_generation")
                .map_err(|error| local_agent_capture_failure("decode local cloud base", error))
        })
        .transpose()
}

pub(super) async fn store_local_agent_capture_cloud_base(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    remote_generation: i64,
) -> CloudResult<()> {
    use sea_orm::Statement;

    db_conn
        .execute_raw(Statement::from_sql_and_values(
            db_conn.get_database_backend(),
            "INSERT INTO agent_capture_cloud_base (repo_id, remote_generation, updated_at)
             VALUES (?, ?, ?)
             ON CONFLICT(repo_id) DO UPDATE SET
                remote_generation = excluded.remote_generation,
                updated_at = excluded.updated_at
             WHERE excluded.remote_generation > agent_capture_cloud_base.remote_generation",
            [
                repo_id.into(),
                remote_generation.into(),
                chrono::Utc::now().timestamp_millis().into(),
            ],
        ))
        .await
        .map_err(|error| local_agent_capture_failure("record local cloud base", error))?;
    Ok(())
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct AgentCaptureSnapshot {
    pub(super) sessions: Vec<AgentSessionV2Row>,
    pub(super) checkpoints: Vec<AgentCheckpointV2Row>,
    pub(super) claims: Vec<AgentSubagentContentClaimRow>,
    pub(super) revisions: Vec<AgentSubagentContentRevisionRow>,
    pub(super) links: Vec<AgentSubagentLinkRow>,
    pub(super) prune_tombstones: Vec<AgentCheckpointPruneTombstoneRow>,
    /// PD-03 session-erasure tombstones from the local
    /// `agent_import_tombstone` table.
    pub(super) import_tombstones: Vec<AgentImportTombstoneRow>,
    pub(super) required_oids: HashSet<String>,
    pub(super) traces_head: Option<String>,
    /// Local coverage revisions that bind an old Claude checkpoint to ordinary
    /// transcript content. They are not a D1 table. Cloud sync still requires
    /// each one to keep its old checkpoint reference, and refuses a tombstoned
    /// session that still has the replacement.
    pub(super) claude_legacy_revisions: Vec<ClaudeLegacyReasoningMirrorRef>,
}

pub(super) async fn validated_claude_legacy_reasoning_mirror_refs(
    db_conn: &sea_orm::DatabaseConnection,
) -> Result<Vec<ClaudeLegacyReasoningMirrorRef>, String> {
    let catalog_conn = local_capture_catalog(db_conn)
        .await
        .map_err(cloud_error_text)?;
    let rows = load_claude_legacy_revision_rows(&catalog_conn)
        .await
        .map_err(cloud_error_text)?;
    validate_claude_legacy_revision_mirror(&rows).map_err(cloud_error_text)
}

fn cloud_error_text(error: CloudError) -> String {
    match error {
        CloudError::PartialTransfer(detail)
        | CloudError::Generic(detail)
        | CloudError::D1(detail)
        | CloudError::R2(detail)
        | CloudError::NameAlreadyTaken(detail)
        | CloudError::NameNotFound(detail)
        | CloudError::AmbiguousObjectFormat(detail)
        | CloudError::RemoteCaptureKindRefused(detail)
        | CloudError::LocalCaptureKindRefused(detail) => detail,
        CloudError::MissingEnv { detail, .. } => detail,
        other @ CloudError::ObjectFormatMismatch { .. } => other.to_string(),
    }
}

/// The legacy Claude reasoning revisions, read whole for
/// [`validated_claude_legacy_reasoning_mirror_refs`]. Their key is the
/// revision table's primary key — with `coverage_schema_version` fixed and
/// `logical_turn_key` following from `checkpoint_id`, the (session,
/// checkpoint, revision) order. The unary `+` keeps the fixed version from
/// counting as an index equality, which would make SQLite sort the last key
/// term instead of walking the index. The freeze reads them through
/// [`claude_legacy_page_sql`] instead.
pub(super) const CLAUDE_LEGACY_REVISION_QUERY: CatalogQuery = CatalogQuery {
    label: "Claude legacy reasoning revision",
    select: "SELECT r.checkpoint_id, r.session_id, r.coverage_digest, c.tree_oid,
        CASE WHEN t.agent_kind IS NULL THEN 0 ELSE 1 END AS tombstoned,
        r.logical_turn_key, r.coverage_schema_version, r.revision
 FROM agent_coverage_revision r
 JOIN agent_session s ON s.session_id = r.session_id
 LEFT JOIN agent_checkpoint c
   ON c.checkpoint_id = r.checkpoint_id AND c.session_id = r.session_id
 LEFT JOIN agent_import_tombstone t
   ON t.agent_kind = s.agent_kind
  AND t.provider_session_id = s.provider_session_id",
    filter: "r.logical_turn_key = 'claude-legacy:' || r.checkpoint_id
   AND +r.coverage_schema_version = 2",
    key: &[
        KeyColumn::Text("r.session_id"),
        KeyColumn::Text("r.logical_turn_key"),
        KeyColumn::Integer("r.coverage_schema_version"),
        KeyColumn::Integer("r.revision"),
    ],
};

async fn claude_legacy_revisions_present(
    db_conn: &impl sea_orm::ConnectionTrait,
) -> CloudResult<bool> {
    use sea_orm::Statement;

    Ok(db_conn
        .query_one_raw(Statement::from_string(
            db_conn.get_database_backend(),
            "SELECT 1 AS present FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_coverage_revision' LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(|error| {
            local_agent_capture_failure("probe Claude legacy reasoning revision schema", error)
        })?
        .is_some())
}

async fn load_claude_legacy_revision_rows(
    db_conn: &impl sea_orm::ConnectionTrait,
) -> CloudResult<Vec<ClaudeLegacyRevisionMirrorRow>> {
    use sea_orm::Statement;

    if !claude_legacy_revisions_present(db_conn).await? {
        return Ok(Vec::new());
    }
    let rows = db_conn
        .query_all_raw(Statement::from_string(
            db_conn.get_database_backend(),
            catalog_query_sql(&CLAUDE_LEGACY_REVISION_QUERY, false, false),
        ))
        .await
        .map_err(|error| {
            local_agent_capture_failure("read Claude legacy reasoning revisions", error)
        })?;
    decode_claude_legacy_revision_rows(rows)
}

/// One page of the coverage revision table in primary-key order — at most
/// `LIMIT ?` rows, after a key (`?` per key column) when `after` is set — each
/// row flagged `legacy` when it is a legacy Claude reasoning revision of an
/// existing session, with the facts the mirror needs. Rows are filtered after
/// the page is read, so every statement reads one page of the index however
/// few legacy rows the table holds.
pub(super) fn claude_legacy_page_sql(after: bool) -> String {
    let keyset = if after {
        " WHERE (session_id, logical_turn_key, coverage_schema_version, revision) > (?, ?, ?, ?)"
    } else {
        ""
    };
    format!(
        "SELECT r.session_id, r.logical_turn_key, r.coverage_schema_version, r.revision,
        r.checkpoint_id, r.coverage_digest, c.tree_oid,
        CASE WHEN t.agent_kind IS NULL THEN 0 ELSE 1 END AS tombstoned,
        CASE WHEN s.session_id IS NOT NULL
              AND r.logical_turn_key = 'claude-legacy:' || r.checkpoint_id
              AND r.coverage_schema_version = 2
             THEN 1 ELSE 0 END AS legacy
 FROM (SELECT session_id, logical_turn_key, coverage_schema_version, revision,
              checkpoint_id, coverage_digest
       FROM agent_coverage_revision{keyset}
       ORDER BY session_id, logical_turn_key, coverage_schema_version, revision
       LIMIT ?) r
 LEFT JOIN agent_session s ON s.session_id = r.session_id
 LEFT JOIN agent_checkpoint c
   ON c.checkpoint_id = r.checkpoint_id AND c.session_id = r.session_id
 LEFT JOIN agent_import_tombstone t
   ON t.agent_kind = s.agent_kind
  AND t.provider_session_id = s.provider_session_id"
    )
}

/// The legacy Claude reasoning revisions, one [`claude_legacy_page_sql`]
/// page of the revision table per statement (no transaction), in key order,
/// charging the shared restore row budget and calling `after_page` after each
/// page.
async fn load_claude_legacy_revision_pages<C: ConnectionTrait>(
    conn: &C,
    remaining_rows: &mut usize,
    after_page: Option<&CatalogPageHook>,
) -> CloudResult<Vec<ClaudeLegacyRevisionMirrorRow>> {
    type RevisionKey = (String, String, i64, i64);
    let decode_failure =
        |error| local_agent_capture_failure("decode Claude legacy reasoning revisions", error);
    let mut rows = Vec::new();
    let mut after: Option<RevisionKey> = None;
    loop {
        let mut values: Vec<sea_orm::Value> = Vec::new();
        if let Some((session_id, turn_key, version, revision)) = &after {
            values.extend([
                session_id.clone().into(),
                turn_key.clone().into(),
                (*version).into(),
                (*revision).into(),
            ]);
        }
        values.push(
            i64::try_from(AGENT_CAPTURE_LOCAL_PAGE_SIZE)
                .map_err(|error| {
                    local_agent_capture_failure("encode local catalog page size", error)
                })?
                .into(),
        );
        let page = conn
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                claude_legacy_page_sql(after.is_some()),
                values,
            ))
            .await
            .map_err(|error| {
                local_agent_capture_failure("read Claude legacy reasoning revisions", error)
            })?;
        let page_len = page.len();
        let mut keyed = page
            .into_iter()
            .map(|row| {
                let key: RevisionKey = (
                    row.try_get_by("session_id")?,
                    row.try_get_by("logical_turn_key")?,
                    row.try_get_by("coverage_schema_version")?,
                    row.try_get_by("revision")?,
                );
                Ok((key, row))
            })
            .collect::<Result<Vec<_>, sea_orm::DbErr>>()
            .map_err(decode_failure)?;
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        if let Some((key, _)) = keyed.last() {
            after = Some(key.clone());
        }
        let mut legacy = Vec::new();
        for (_, row) in keyed {
            if row.try_get_by::<i64, _>("legacy").map_err(decode_failure)? != 0 {
                legacy.push(row);
            }
        }
        if legacy.len() > *remaining_rows {
            return Err(CloudError::PartialTransfer(format!(
                "local agent-capture catalog exceeds the aggregate {}-row restore safety bound while reading Claude legacy reasoning revision",
                AGENT_CAPTURE_RESTORE_MAX_ROWS
            )));
        }
        *remaining_rows -= legacy.len();
        rows.extend(decode_claude_legacy_revision_rows(legacy)?);
        if let Some(after_page) = after_page {
            after_page().await;
        }
        if page_len < AGENT_CAPTURE_LOCAL_PAGE_SIZE {
            break;
        }
    }
    Ok(rows)
}

fn decode_claude_legacy_revision_rows(
    rows: Vec<sea_orm::QueryResult>,
) -> CloudResult<Vec<ClaudeLegacyRevisionMirrorRow>> {
    rows.into_iter()
        .map(|row| {
            Ok(ClaudeLegacyRevisionMirrorRow {
                checkpoint_id: row.try_get_by("checkpoint_id")?,
                session_id: row.try_get_by("session_id")?,
                content_digest: row.try_get_by("coverage_digest")?,
                tree_oid: row.try_get_by("tree_oid")?,
                tombstoned: row.try_get_by::<i64, _>("tombstoned")? != 0,
            })
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()
        .map_err(|error| {
            local_agent_capture_failure("decode Claude legacy reasoning revisions", error)
        })
}

fn validate_claude_legacy_revision_mirror(
    rows: &[ClaudeLegacyRevisionMirrorRow],
) -> CloudResult<Vec<ClaudeLegacyReasoningMirrorRef>> {
    let mut refs = Vec::with_capacity(rows.len());
    for row in rows {
        if row.tombstoned {
            return Err(CloudError::PartialTransfer(
                "a tombstoned Claude session still has a legacy reasoning revision; finish the erase before cloud sync"
                    .to_string(),
            ));
        }
        let Some(tree_oid) = row.tree_oid.clone().filter(|oid| !oid.is_empty()) else {
            return Err(CloudError::PartialTransfer(
                "a Claude legacy reasoning revision is missing its old checkpoint; refusing to mirror the replacement alone"
                    .to_string(),
            ));
        };
        refs.push(ClaudeLegacyReasoningMirrorRef {
            checkpoint_id: row.checkpoint_id.clone(),
            session_id: row.session_id.clone(),
            content_digest: row.content_digest.clone(),
            tree_oid,
        });
    }
    Ok(refs)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct ClaudeLegacyRevisionMirrorRow {
    checkpoint_id: String,
    session_id: String,
    content_digest: String,
    tree_oid: Option<String>,
    tombstoned: bool,
}

/// PD-03: union local and remote session tombstones, keeping the newest
/// `erased_at` per provider identity. `source_fingerprint` is retired and
/// deliberately omitted from the merged wire state; delete/restore replays
/// stay idempotent without retaining legacy source metadata.
pub(super) fn merge_import_tombstones(
    local: &[AgentImportTombstoneRow],
    remote: &[AgentImportTombstoneRow],
) -> Vec<AgentImportTombstoneRow> {
    let mut merged: std::collections::BTreeMap<(String, String), AgentImportTombstoneRow> =
        std::collections::BTreeMap::new();
    for row in remote.iter().chain(local.iter()) {
        let key = (row.agent_kind.clone(), row.provider_session_id.clone());
        let mut sanitized = row.clone();
        sanitized.source_fingerprint = None;
        match merged.get_mut(&key) {
            None => {
                merged.insert(key, sanitized);
            }
            Some(existing) => {
                if sanitized.erased_at > existing.erased_at {
                    *existing = sanitized;
                }
            }
        }
    }
    merged.into_values().collect()
}

fn agent_capture_catalog_row_count(snapshot: &AgentCaptureSnapshot) -> CloudResult<usize> {
    [
        snapshot.sessions.len(),
        snapshot.checkpoints.len(),
        snapshot.prune_tombstones.len(),
        snapshot.import_tombstones.len(),
        snapshot.claims.len(),
        snapshot.revisions.len(),
        snapshot.links.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, count| {
        total.checked_add(count).ok_or_else(|| {
            CloudError::PartialTransfer(
                "agent-capture catalog row count exceeds the platform size range".to_string(),
            )
        })
    })
}

fn validate_agent_capture_restore_row_budget(
    snapshot: &AgentCaptureSnapshot,
    object_index_rows: usize,
) -> CloudResult<()> {
    let total = agent_capture_catalog_row_count(snapshot)?
        .checked_add(object_index_rows)
        .ok_or_else(|| {
            CloudError::PartialTransfer(
                "agent-capture restore row count exceeds the platform size range".to_string(),
            )
        })?;
    if total > AGENT_CAPTURE_RESTORE_MAX_ROWS {
        return Err(CloudError::PartialTransfer(format!(
            "agent-capture catalog and object manifest require {total} rows, exceeding the aggregate {}-row restore safety bound",
            AGENT_CAPTURE_RESTORE_MAX_ROWS
        )));
    }
    Ok(())
}

pub(crate) struct AgentCaptureRestoreRows<'a> {
    pub(super) sessions: &'a [AgentSessionV2Row],
    pub(super) checkpoints: &'a [AgentCheckpointV2Row],
    pub(super) claims: &'a [AgentSubagentContentClaimRow],
    pub(super) revisions: &'a [AgentSubagentContentRevisionRow],
    pub(super) links: &'a [AgentSubagentLinkRow],
    pub(super) traces_head: Option<&'a str>,
    /// Numeric row revisions are meaningful only when this clone recorded the
    /// completed remote generation as its base. Without that lineage proof, a
    /// larger local counter may be an unrelated clone's divergent history.
    pub(super) remote_is_known_ancestor: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentCaptureObjectManifestScope {
    CheckpointProjection,
    FullRemoteIndex,
}

impl AgentCaptureObjectManifestScope {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointProjection => "checkpoint_projection",
            Self::FullRemoteIndex => "full_remote_index",
        }
    }

    pub(super) fn parse(value: Option<&str>) -> CloudResult<Self> {
        match value {
            Some("checkpoint_projection") => Ok(Self::CheckpointProjection),
            Some("full_remote_index") => Ok(Self::FullRemoteIndex),
            _ => Err(CloudError::PartialTransfer(
                "agent-capture generation has no supported object-index scope; run a current-version cloud sync"
                    .to_string(),
            )),
        }
    }
}

pub(super) fn agent_capture_object_index_digest(
    indexes: &[ObjectIndexRow],
) -> CloudResult<(String, i64)> {
    let mut rows = indexes.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| left.o_id.cmp(&right.o_id));
    let mut previous: Option<&str> = None;
    let mut digest = Sha256::new();
    for row in rows {
        if previous == Some(row.o_id.as_str()) {
            return Err(CloudError::PartialTransfer(
                "remote agent-capture object index contains duplicate object ids; run `libra cloud sync` and retry"
                    .to_string(),
            ));
        }
        previous = Some(row.o_id.as_str());
        for value in [row.o_id.as_bytes(), row.o_type.as_bytes()] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        digest.update(row.o_size.to_be_bytes());
    }
    let count = i64::try_from(indexes.len()).map_err(|error| {
        local_agent_capture_failure("encode object-index manifest count", error)
    })?;
    Ok((hex::encode(digest.finalize()), count))
}

pub(super) fn validate_checkpoint_object_index_roots(
    checkpoints: &[AgentCheckpointV2Row],
    indexes: &[ObjectIndexRow],
    _side: &str,
) -> CloudResult<()> {
    let indexed_oids = indexes
        .iter()
        .map(|row| row.o_id.as_str())
        .collect::<HashSet<_>>();
    for checkpoint in checkpoints {
        for oid in [
            checkpoint.traces_commit.as_str(),
            checkpoint.tree_oid.as_str(),
            checkpoint.metadata_blob_oid.as_str(),
        ] {
            if !indexed_oids.contains(oid) {
                return Err(CloudError::PartialTransfer(
                    "remote agent-capture checkpoint references an object absent from the fenced object index; run `libra cloud sync` and retry"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Runs after each page of a frozen catalog read (tests drive concurrent
/// writers through it; PI-26).
pub(super) type CatalogPageHook =
    std::sync::Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// One column of a catalog key: its SQL expression, read back from the
/// result column of the same name (the part after any table alias).
#[derive(Debug, Clone, Copy)]
pub(super) enum KeyColumn {
    Text(&'static str),
    Integer(&'static str),
}

impl KeyColumn {
    fn expression(self) -> &'static str {
        match self {
            KeyColumn::Text(expression) | KeyColumn::Integer(expression) => expression,
        }
    }

    fn value_in(self, row: &sea_orm::QueryResult) -> Result<sea_orm::Value, sea_orm::DbErr> {
        let expression = self.expression();
        let column = expression.rsplit('.').next().unwrap_or(expression);
        match self {
            KeyColumn::Text(_) => row.try_get_by::<String, _>(column).map(Into::into),
            KeyColumn::Integer(_) => row.try_get_by::<i64, _>(column).map(Into::into),
        }
    }
}

/// A local catalog table read a keyset page at a time (PI-26): its columns,
/// an optional filter, and a unique key whose order an index provides, so
/// every page is one short indexed statement wherever it starts.
#[derive(Debug)]
pub(super) struct CatalogQuery {
    pub(super) label: &'static str,
    pub(super) select: &'static str,
    pub(super) filter: &'static str,
    pub(super) key: &'static [KeyColumn],
}

/// `query` in key order: optionally only the rows after a key (`?` per key
/// column), optionally one page (`LIMIT ?`).
pub(super) fn catalog_query_sql(query: &CatalogQuery, after: bool, page: bool) -> String {
    let columns = query
        .key
        .iter()
        .map(|column| column.expression())
        .collect::<Vec<_>>()
        .join(", ");
    let mut conditions = Vec::new();
    if !query.filter.is_empty() {
        conditions.push(query.filter.to_string());
    }
    if after {
        let placeholders = vec!["?"; query.key.len()].join(", ");
        conditions.push(format!("({columns}) > ({placeholders})"));
    }
    let filter = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    let limit = if page { " LIMIT ?" } else { "" };
    format!("{}{filter} ORDER BY {columns}{limit}", query.select)
}

pub(super) const AGENT_SESSION_QUERY: CatalogQuery = CatalogQuery {
    label: "agent session",
    select: "SELECT session_id, agent_kind, provider_session_id, state, working_dir,
                worktree_id, parent_commit, parent_session_id, metadata_json,
                redaction_report, started_at, last_event_at, stopped_at, schema_version,
                sync_revision
         FROM agent_session",
    filter: "",
    key: &[KeyColumn::Text("session_id")],
};

/// Read in key order; [`read_agent_capture_catalog`] restores the
/// (`created_at`, `checkpoint_id`) order no index provides.
pub(super) const AGENT_CHECKPOINT_QUERY: CatalogQuery = CatalogQuery {
    label: "agent checkpoint",
    select: "SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at, sync_revision
         FROM agent_checkpoint",
    filter: "",
    key: &[KeyColumn::Text("checkpoint_id")],
};

pub(super) const PRUNE_TOMBSTONE_QUERY: CatalogQuery = CatalogQuery {
    label: "checkpoint prune tombstone",
    select: "SELECT checkpoint_id, session_id, pruned_at FROM agent_checkpoint_prune_tombstone",
    filter: "",
    key: &[KeyColumn::Text("checkpoint_id")],
};

pub(super) const IMPORT_TOMBSTONE_QUERY: CatalogQuery = CatalogQuery {
    label: "agent import tombstone",
    select: "SELECT agent_kind, provider_session_id, erased_session_id, erased_at
             FROM agent_import_tombstone",
    filter: "",
    key: &[
        KeyColumn::Text("agent_kind"),
        KeyColumn::Text("provider_session_id"),
    ],
};

pub(super) const SUBAGENT_CLAIM_QUERY: CatalogQuery = CatalogQuery {
    label: "subagent content claim",
    select: "SELECT parent_session_id, provider_kind, source_key,
                    content_schema_version, revision_cursor, sync_revision, current_revision,
                    current_checkpoint_id, current_digest, fence_token, created_at, updated_at
             FROM agent_subagent_content_claim",
    filter: "",
    key: &[
        KeyColumn::Text("parent_session_id"),
        KeyColumn::Text("provider_kind"),
        KeyColumn::Text("source_key"),
        KeyColumn::Integer("content_schema_version"),
    ],
};

pub(super) const SUBAGENT_REVISION_QUERY: CatalogQuery = CatalogQuery {
    label: "subagent content revision",
    select: "SELECT parent_session_id, provider_kind, source_key,
                    content_schema_version, revision, checkpoint_id, content_digest,
                    source_channel, partial, created_at
             FROM agent_subagent_content_revision",
    filter: "",
    key: &[
        KeyColumn::Text("parent_session_id"),
        KeyColumn::Text("provider_kind"),
        KeyColumn::Text("source_key"),
        KeyColumn::Integer("content_schema_version"),
        KeyColumn::Integer("revision"),
    ],
};

/// Read in key order; [`read_agent_capture_catalog`] restores the
/// (`created_at`, `content_checkpoint_id`) order no index provides.
pub(super) const SUBAGENT_LINK_QUERY: CatalogQuery = CatalogQuery {
    label: "subagent association link",
    select: "SELECT content_checkpoint_id, parent_session_id, link_state,
                    boundary_checkpoint_id, stable_subagent_id, sync_revision,
                    created_at, updated_at
             FROM agent_subagent_link",
    filter: "",
    key: &[KeyColumn::Text("content_checkpoint_id")],
};

/// Every catalog query the freeze pages through with
/// [`load_local_capture_keyset_pages`] (the legacy Claude rows go through
/// [`claude_legacy_page_sql`]).
#[cfg(test)]
pub(super) const CATALOG_QUERIES: [&CatalogQuery; 7] = [
    &AGENT_SESSION_QUERY,
    &AGENT_CHECKPOINT_QUERY,
    &PRUNE_TOMBSTONE_QUERY,
    &IMPORT_TOMBSTONE_QUERY,
    &SUBAGENT_CLAIM_QUERY,
    &SUBAGENT_REVISION_QUERY,
    &SUBAGENT_LINK_QUERY,
];

/// The rows of `query` a keyset page at a time, each page its own short
/// indexed statement (no transaction), charging the shared restore row
/// budget and calling `after_page` after each page.
pub(super) async fn load_local_capture_keyset_pages<C: ConnectionTrait>(
    conn: &C,
    query: &CatalogQuery,
    remaining_rows: &mut usize,
    after_page: Option<&CatalogPageHook>,
) -> CloudResult<Vec<sea_orm::QueryResult>> {
    let mut rows = Vec::new();
    let mut after: Option<Vec<sea_orm::Value>> = None;
    loop {
        // Read one sentinel row beyond the shared remaining budget so an
        // aggregate overflow fails before the generation can be advertised.
        let page_limit = AGENT_CAPTURE_LOCAL_PAGE_SIZE.min(remaining_rows.saturating_add(1));
        let mut values = after.clone().unwrap_or_default();
        values.push(
            i64::try_from(page_limit)
                .map_err(|error| {
                    local_agent_capture_failure("encode local catalog page size", error)
                })?
                .into(),
        );
        let page = conn
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                catalog_query_sql(query, after.is_some(), true),
                values,
            ))
            .await
            .map_err(|error| local_agent_capture_failure("read local catalog page", error))?;
        let page_len = page.len();
        if page_len > *remaining_rows {
            return Err(CloudError::PartialTransfer(format!(
                "local agent-capture catalog exceeds the aggregate {}-row restore safety bound while reading {}",
                AGENT_CAPTURE_RESTORE_MAX_ROWS, query.label
            )));
        }
        *remaining_rows -= page_len;
        if let Some(last) = page.last() {
            after = Some(
                query
                    .key
                    .iter()
                    .map(|column| column.value_in(last))
                    .collect::<Result<_, _>>()
                    .map_err(|error| {
                        local_agent_capture_failure("decode local catalog page key", error)
                    })?,
            );
        }
        rows.extend(page);
        if let Some(after_page) = after_page {
            after_page().await;
        }
        if page_len < page_limit {
            break;
        }
    }
    Ok(rows)
}

pub(super) async fn load_synced_required_object_oids<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<HashSet<String>> {
    if required_oids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
        return Err(CloudError::Generic(format!(
            "agent checkpoint reachability exceeds the {}-object cloud safety bound",
            AGENT_CAPTURE_MAX_ROWS_PER_TABLE
        )));
    }
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut synced = HashSet::with_capacity(required.len());
    for page in required.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let placeholders = vec!["?"; page.len()].join(", ");
        let mut values = Vec::with_capacity(page.len().saturating_add(1));
        values.push(repo_id.into());
        values.extend(page.iter().cloned().map(Into::into));
        let rows = conn
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!(
                    "SELECT o_id FROM object_index
                     WHERE repo_id = ? AND is_synced = 1 AND o_id IN ({placeholders})"
                ),
                values,
            ))
            .await
            .map_err(|error| {
                local_agent_capture_failure("read checkpoint-reachable object indexes", error)
            })?;
        for row in rows {
            synced.insert(row.try_get_by::<String, _>("o_id").map_err(|error| {
                local_agent_capture_failure("decode checkpoint-reachable object index", error)
            })?);
        }
    }
    Ok(synced)
}

async fn load_required_local_object_indexes(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<HashMap<String, object_index::Model>> {
    if required_oids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
        return Err(CloudError::Generic(format!(
            "agent checkpoint reachability exceeds the {}-object cloud safety bound",
            AGENT_CAPTURE_MAX_ROWS_PER_TABLE
        )));
    }
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut rows = HashMap::with_capacity(required.len());
    for page in required.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let models = object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(repo_id))
            .filter(object_index::Column::OId.is_in(page.iter().cloned()))
            .all(db_conn)
            .await
            .map_err(|error| {
                local_agent_capture_failure("load checkpoint-reachable local object indexes", error)
            })?;
        rows.extend(models.into_iter().map(|model| (model.o_id.clone(), model)));
    }
    Ok(rows)
}

/// The agent-capture catalog as frozen with a sync's snapshot (PI-26): the
/// raw local rows, read with short statements inside the freeze window and
/// validated, fenced and checked against the current catalog only when
/// published.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum FrozenAgentCapture {
    /// The repository predates the agent-capture tables.
    LegacySchema,
    /// The catalog could not be read; the mirror phase reports why.
    Unreadable { reason: String },
    /// The frozen rows.
    Catalog(Box<FrozenAgentCaptureCatalog>),
}

/// Raw local agent-capture rows of one consistent view (PI-26).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct FrozenAgentCaptureCatalog {
    pub(super) subagent_content_present: bool,
    pub(super) sessions: Vec<AgentSessionV2Row>,
    pub(super) checkpoints: Vec<AgentCheckpointV2Row>,
    pub(super) prune_tombstones: Vec<AgentCheckpointPruneTombstoneRow>,
    pub(super) import_tombstones: Vec<AgentImportTombstoneRow>,
    pub(super) traces_head: Option<String>,
    pub(super) claims: Vec<AgentSubagentContentClaimRow>,
    pub(super) revisions: Vec<AgentSubagentContentRevisionRow>,
    pub(super) links: Vec<AgentSubagentLinkRow>,
    pub(super) claude_legacy_rows: Vec<ClaudeLegacyRevisionMirrorRow>,
}

/// Freeze the local agent-capture catalog on `conn` (PI-26): each page is
/// its own short statement, so no lock is held across the read; the
/// caller's `data_version` check proves the pages form one view. Read
/// failures are kept for the mirror phase to report, as before, rather than
/// failing the whole sync.
pub(super) async fn freeze_agent_capture<C: ConnectionTrait>(
    conn: &C,
    after_page: Option<&CatalogPageHook>,
) -> FrozenAgentCapture {
    match probe_agent_capture_schema(conn).await {
        Ok((false, _)) => FrozenAgentCapture::LegacySchema,
        Ok((true, subagent)) => {
            match read_agent_capture_catalog(conn, subagent, after_page).await {
                Ok(catalog) => FrozenAgentCapture::Catalog(Box::new(catalog)),
                Err(error) => FrozenAgentCapture::Unreadable {
                    reason: error.to_string(),
                },
            }
        }
        Err(error) => FrozenAgentCapture::Unreadable {
            reason: error.to_string(),
        },
    }
}

/// Whether the agent-session and the subagent-content tables exist.
async fn probe_agent_capture_schema<C: ConnectionTrait>(conn: &C) -> CloudResult<(bool, bool)> {
    use sea_orm::Statement;

    let present = |table: &'static str| {
        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
            [table.into()],
        ))
    };
    let session = present("agent_session")
        .await
        .map_err(|error| local_agent_capture_failure("probe local session schema", error))?
        .is_some();
    let subagent = present("agent_subagent_content_claim")
        .await
        .map_err(|error| local_agent_capture_failure("probe local subagent schema", error))?
        .is_some();
    Ok((session, subagent))
}

/// Read the local agent-capture catalog projection on `conn`: the rows, one
/// short statement per page (no transaction), with sessions projected for
/// the cloud so no field the cloud never receives is frozen. Shared by the
/// freeze and by the publication-time stability check, so both describe the
/// catalog the same way.
pub(super) async fn read_agent_capture_catalog<C: ConnectionTrait>(
    conn: &C,
    subagent_content_present: bool,
    after_page: Option<&CatalogPageHook>,
) -> CloudResult<FrozenAgentCaptureCatalog> {
    use sea_orm::Statement;

    let txn = conn;
    let backend = txn.get_database_backend();
    let mut remaining_restore_rows = AGENT_CAPTURE_RESTORE_MAX_ROWS;

    let session_rows = load_local_capture_keyset_pages(
        txn,
        &AGENT_SESSION_QUERY,
        &mut remaining_restore_rows,
        after_page,
    )
    .await?;
    let sessions: Vec<AgentSessionV2Row> = session_rows
        .into_iter()
        .map(|row| {
            Ok(AgentSessionV2Row {
                session_id: row.try_get_by("session_id")?,
                agent_kind: row.try_get_by("agent_kind")?,
                provider_session_id: row.try_get_by("provider_session_id")?,
                state: row.try_get_by("state")?,
                working_dir: row.try_get_by("working_dir")?,
                worktree_id: row.try_get_by("worktree_id")?,
                parent_commit: row.try_get_by("parent_commit")?,
                parent_session_id: row.try_get_by("parent_session_id")?,
                metadata_json: row.try_get_by("metadata_json")?,
                redaction_report: row.try_get_by("redaction_report")?,
                started_at: row.try_get_by("started_at")?,
                last_event_at: row.try_get_by("last_event_at")?,
                stopped_at: row.try_get_by("stopped_at")?,
                schema_version: row.try_get_by("schema_version")?,
                sync_revision: row.try_get_by("sync_revision")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(|error| {
            local_agent_capture_failure("decode local agent-session snapshot", error)
        })?;

    let checkpoint_rows = load_local_capture_keyset_pages(
        txn,
        &AGENT_CHECKPOINT_QUERY,
        &mut remaining_restore_rows,
        after_page,
    )
    .await?;
    let checkpoints: Vec<AgentCheckpointV2Row> = checkpoint_rows
        .into_iter()
        .map(|row| {
            Ok(AgentCheckpointV2Row {
                checkpoint_id: row.try_get_by("checkpoint_id")?,
                session_id: row.try_get_by("session_id")?,
                parent_checkpoint_id: row.try_get_by("parent_checkpoint_id")?,
                scope: row.try_get_by("scope")?,
                parent_commit: row.try_get_by("parent_commit")?,
                tree_oid: row.try_get_by("tree_oid")?,
                metadata_blob_oid: row.try_get_by("metadata_blob_oid")?,
                traces_commit: row.try_get_by("traces_commit")?,
                tool_use_id: row.try_get_by("tool_use_id")?,
                subagent_session_id: row.try_get_by("subagent_session_id")?,
                description: row.try_get_by("description")?,
                created_at: row.try_get_by("created_at")?,
                sync_revision: row.try_get_by("sync_revision")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(|error| local_agent_capture_failure("decode local checkpoint snapshot", error))?;
    let mut checkpoints = checkpoints;
    checkpoints.sort_by(|left, right| {
        (left.created_at, &left.checkpoint_id).cmp(&(right.created_at, &right.checkpoint_id))
    });

    let prune_tombstones = if subagent_content_present {
        let tombstone_rows = load_local_capture_keyset_pages(
            txn,
            &PRUNE_TOMBSTONE_QUERY,
            &mut remaining_restore_rows,
            after_page,
        )
        .await?;
        tombstone_rows
            .into_iter()
            .map(|row| {
                Ok(AgentCheckpointPruneTombstoneRow {
                    checkpoint_id: row.try_get_by("checkpoint_id")?,
                    session_id: row.try_get_by("session_id")?,
                    pruned_at: row.try_get_by("pruned_at")?,
                })
            })
            .collect::<Result<Vec<_>, sea_orm::DbErr>>()
            .map_err(|error| {
                local_agent_capture_failure("decode local checkpoint-prune tombstones", error)
            })?
    } else {
        Vec::new()
    };
    // The traces ref is in `libra.db`; a `capture.db` catalog reads it from
    // its paired repository database.
    let head_statement = Statement::from_sql_and_values(
        backend,
        "SELECT `commit` FROM reference
         WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
        [crate::internal::branch::TRACES_BRANCH.into()],
    );
    let traces_head = match crate::internal::db::reference_peer(txn)
        .await
        .map_err(|error| local_agent_capture_failure("open local traces snapshot head", error))?
    {
        Some(repository) => repository.query_one_raw(head_statement).await,
        None => txn.query_one_raw(head_statement).await,
    }
    .map_err(|error| local_agent_capture_failure("read local traces snapshot head", error))?
    .map(|row| row.try_get_by::<Option<String>, _>("commit"))
    .transpose()
    .map_err(|error| local_agent_capture_failure("decode local traces snapshot head", error))?
    .flatten();

    let import_tombstone_present = txn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_import_tombstone' LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(|error| local_agent_capture_failure("probe local import-tombstone schema", error))?
        .is_some();
    let import_tombstones = if import_tombstone_present {
        let tombstone_rows = load_local_capture_keyset_pages(
            txn,
            &IMPORT_TOMBSTONE_QUERY,
            &mut remaining_restore_rows,
            after_page,
        )
        .await?;
        tombstone_rows
            .into_iter()
            .map(|row| {
                Ok(AgentImportTombstoneRow {
                    agent_kind: row.try_get_by("agent_kind")?,
                    provider_session_id: row.try_get_by("provider_session_id")?,
                    erased_session_id: row.try_get_by("erased_session_id")?,
                    // Legacy local databases can still contain this retired
                    // column. It must not escape in a capture snapshot.
                    source_fingerprint: None,
                    erased_at: row.try_get_by("erased_at")?,
                })
            })
            .collect::<Result<Vec<_>, sea_orm::DbErr>>()
            .map_err(|error| local_agent_capture_failure("decode local import tombstones", error))?
    } else {
        Vec::new()
    };
    let mut catalog = FrozenAgentCaptureCatalog {
        subagent_content_present,
        sessions,
        checkpoints,
        prune_tombstones,
        import_tombstones,
        traces_head,
        ..FrozenAgentCaptureCatalog::default()
    };
    if subagent_content_present {
        let claim_rows = load_local_capture_keyset_pages(
            txn,
            &SUBAGENT_CLAIM_QUERY,
            &mut remaining_restore_rows,
            after_page,
        )
        .await?;
        catalog.claims = claim_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentContentClaimRow {
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    provider_kind: row.try_get_by("provider_kind")?,
                    source_key: row.try_get_by("source_key")?,
                    content_schema_version: row.try_get_by("content_schema_version")?,
                    revision_cursor: row.try_get_by("revision_cursor")?,
                    sync_revision: row.try_get_by("sync_revision")?,
                    current_revision: row.try_get_by("current_revision")?,
                    current_checkpoint_id: row.try_get_by("current_checkpoint_id")?,
                    current_digest: row.try_get_by("current_digest")?,
                    fence_token: row.try_get_by("fence_token")?,
                    created_at: row.try_get_by("created_at")?,
                    updated_at: row.try_get_by("updated_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                local_agent_capture_failure("decode local subagent-claim snapshot", error)
            })?;
        let revision_rows = load_local_capture_keyset_pages(
            txn,
            &SUBAGENT_REVISION_QUERY,
            &mut remaining_restore_rows,
            after_page,
        )
        .await?;
        catalog.revisions = revision_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentContentRevisionRow {
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    provider_kind: row.try_get_by("provider_kind")?,
                    source_key: row.try_get_by("source_key")?,
                    content_schema_version: row.try_get_by("content_schema_version")?,
                    revision: row.try_get_by("revision")?,
                    checkpoint_id: row.try_get_by("checkpoint_id")?,
                    content_digest: row.try_get_by("content_digest")?,
                    source_channel: row.try_get_by("source_channel")?,
                    partial: row.try_get_by("partial")?,
                    created_at: row.try_get_by("created_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                local_agent_capture_failure("decode local subagent-revision snapshot", error)
            })?;
        let link_rows = load_local_capture_keyset_pages(
            txn,
            &SUBAGENT_LINK_QUERY,
            &mut remaining_restore_rows,
            after_page,
        )
        .await?;
        catalog.links = link_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentLinkRow {
                    content_checkpoint_id: row.try_get_by("content_checkpoint_id")?,
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    link_state: row.try_get_by("link_state")?,
                    boundary_checkpoint_id: row.try_get_by("boundary_checkpoint_id")?,
                    stable_subagent_id: row.try_get_by("stable_subagent_id")?,
                    sync_revision: row.try_get_by("sync_revision")?,
                    created_at: row.try_get_by("created_at")?,
                    updated_at: row.try_get_by("updated_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                local_agent_capture_failure("decode local subagent-link snapshot", error)
            })?;
        catalog.links.sort_by(|left, right| {
            (left.created_at, &left.content_checkpoint_id)
                .cmp(&(right.created_at, &right.content_checkpoint_id))
        });
    }

    if claude_legacy_revisions_present(txn).await? {
        catalog.claude_legacy_rows =
            load_claude_legacy_revision_pages(txn, &mut remaining_restore_rows, after_page).await?;
    }
    for session in &mut catalog.sessions {
        *session = project_agent_session_for_cloud(session, "local")?;
    }
    Ok(catalog)
}

/// The publishable snapshot of a frozen catalog projection: companions,
/// session dependencies and the legacy Claude reasoning references
/// validated.
pub(super) fn agent_capture_snapshot_from_catalog(
    catalog: &FrozenAgentCaptureCatalog,
) -> CloudResult<AgentCaptureSnapshot> {
    let mut snapshot = AgentCaptureSnapshot {
        sessions: catalog.sessions.clone(),
        checkpoints: catalog.checkpoints.clone(),
        claims: catalog.claims.clone(),
        revisions: catalog.revisions.clone(),
        links: catalog.links.clone(),
        prune_tombstones: catalog.prune_tombstones.clone(),
        import_tombstones: catalog.import_tombstones.clone(),
        traces_head: catalog.traces_head.clone(),
        ..AgentCaptureSnapshot::default()
    };
    validate_agent_capture_companions(
        &snapshot.checkpoints,
        &snapshot.claims,
        &snapshot.revisions,
        &snapshot.links,
        "local",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        &snapshot.sessions,
        &snapshot.checkpoints,
        &snapshot.claims,
        "local",
    )?;
    snapshot.claude_legacy_revisions =
        validate_claude_legacy_revision_mirror(&catalog.claude_legacy_rows)?;
    Ok(snapshot)
}

/// [`read_agent_capture_catalog`] followed by
/// [`agent_capture_snapshot_from_catalog`] on the live catalog (tests).
#[cfg(test)]
async fn load_agent_capture_catalog_snapshot(
    db_conn: &sea_orm::DatabaseConnection,
    _repo_id: &str,
    subagent_content_present: bool,
) -> CloudResult<AgentCaptureSnapshot> {
    let catalog_conn = local_capture_catalog(db_conn).await?;
    let catalog = read_agent_capture_catalog(&catalog_conn, subagent_content_present, None).await?;
    agent_capture_snapshot_from_catalog(&catalog)
}

pub(super) fn validate_agent_capture_traces_shape(
    checkpoints: &[AgentCheckpointV2Row],
    traces_head: Option<&str>,
    side: &str,
) -> CloudResult<()> {
    match (checkpoints.is_empty(), traces_head) {
        (true, Some(_)) => Err(CloudError::PartialTransfer(format!(
            "{side} agent-capture catalog is empty but its fenced traces head is nonempty; run `libra agent doctor --repair` before retrying"
        ))),
        (false, None) => Err(CloudError::PartialTransfer(format!(
            "{side} agent-capture catalog has checkpoints but no fenced traces head; run `libra agent doctor --repair` before retrying"
        ))),
        _ => Ok(()),
    }
}

/// Freeze the live catalog and verify it at once, as one sync would (tests).
#[cfg(test)]
pub(super) async fn load_agent_capture_snapshot(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    subagent_content_present: bool,
) -> CloudResult<AgentCaptureSnapshot> {
    let catalog_conn = local_capture_catalog(db_conn).await?;
    let catalog = read_agent_capture_catalog(&catalog_conn, subagent_content_present, None).await?;
    let snapshot = verify_frozen_agent_capture(&catalog, db_conn, repo_id).await?;
    recheck_frozen_agent_capture(&catalog, db_conn, repo_id, None).await?;
    Ok(snapshot)
}

/// The repository-wide object generation gate: no object anywhere in the
/// repository may still be waiting for upload.
async fn ensure_no_unsynced_objects(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
) -> CloudResult<()> {
    use sea_orm::Statement;

    let unsynced = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            db_conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM object_index
             WHERE repo_id = ? AND COALESCE(is_synced, 0) = 0",
            [repo_id.into()],
        ))
        .await
        .map_err(|error| local_agent_capture_failure("verify local object generation", error))?
        .ok_or_else(|| CloudError::Generic("object generation count returned no row".into()))?
        .try_get_by::<i64, _>("n")
        .map_err(|error| local_agent_capture_failure("decode unsynced object count", error))?;
    if unsynced != 0 {
        return Err(CloudError::PartialTransfer(format!(
            "agent capture snapshot found {unsynced} object(s) outside the completed object upload generation; retry `libra cloud sync`"
        )));
    }
    Ok(())
}

/// The local publication checks on a frozen catalog (PI-26), against the
/// current database after the object upload: the repository-wide object
/// generation gate, the companion validation, the traces shape, checkpoint
/// reachability and durability, required objects in the completed upload
/// generation and the restore row budget. Any failure refuses publication;
/// [`recheck_frozen_agent_capture`] runs the stability check last, right
/// before the first remote catalog mutation.
pub(super) async fn verify_frozen_agent_capture(
    catalog: &FrozenAgentCaptureCatalog,
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
) -> CloudResult<AgentCaptureSnapshot> {
    ensure_no_unsynced_objects(db_conn, repo_id).await?;
    let mut snapshot = agent_capture_snapshot_from_catalog(catalog)?;
    validate_agent_capture_traces_shape(
        &snapshot.checkpoints,
        snapshot.traces_head.as_deref(),
        "local",
    )?;

    if cfg!(debug_assertions)
        && let Ok(delay) = std::env::var("LIBRA_TEST_CLOUD_AGENT_SNAPSHOT_DELAY_MS")
        && let Ok(delay) = delay.parse::<u64>()
        && delay > 0
    {
        tokio::time::sleep(std::time::Duration::from_millis(delay.min(5_000))).await;
    }

    let durability_specs = snapshot
        .checkpoints
        .iter()
        .map(
            |checkpoint| crate::internal::ai::history::CheckpointDurabilitySpec {
                checkpoint_id: &checkpoint.checkpoint_id,
                traces_commit: &checkpoint.traces_commit,
                tree_oid: &checkpoint.tree_oid,
                metadata_blob_oid: &checkpoint.metadata_blob_oid,
            },
        )
        .collect::<Vec<_>>();
    let required_oids = if durability_specs.is_empty() {
        HashSet::new()
    } else {
        let traces_head = snapshot.traces_head.as_deref().ok_or_else(|| {
            CloudError::PartialTransfer(
                "local agent-capture snapshot lost its fenced traces head".to_string(),
            )
        })?;
        let cataloged_commits = snapshot
            .checkpoints
            .iter()
            .map(|row| row.traces_commit.clone())
            .collect::<Vec<_>>();
        crate::internal::ai::history::checkpoint_rows_snapshot_durable_oids_from_head(
            &util::storage_path(),
            traces_head,
            &cataloged_commits,
            &durability_specs,
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(110)),
        )
        .await
        .map_err(|_| {
            CloudError::PartialTransfer(
                "agent checkpoint snapshot is not fully reachable and durable; run `libra agent doctor --repair`, then retry cloud sync"
                    .to_string(),
            )
        })?
    };
    let synced_oids = load_synced_required_object_oids(db_conn, repo_id, &required_oids).await?;
    for oid in &required_oids {
        if !synced_oids.contains(oid) {
            return Err(CloudError::PartialTransfer(
                "agent capture cannot be published because a reachable object is not in the completed local object upload generation; run `libra agent doctor --repair`, then retry cloud sync"
                    .to_string(),
            ));
        }
    }
    validate_agent_capture_restore_row_budget(&snapshot, required_oids.len())?;

    snapshot.required_oids = required_oids;
    Ok(snapshot)
}

/// The stability check on a frozen catalog (PI-26), right before the first
/// remote catalog mutation: the current catalog projection, re-read in one
/// unchanged-database window, must still equal the frozen one, and the
/// repository-wide generation gate must still hold. An erasure, tombstone,
/// rewrite or unsynced object since the freeze — also one committed during
/// the upload, the durability verification or the remote object checks — is
/// therefore never published from the snapshot.
pub(super) async fn recheck_frozen_agent_capture(
    catalog: &FrozenAgentCaptureCatalog,
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    after_page: Option<&CatalogPageHook>,
) -> CloudResult<()> {
    let changed = || {
        CloudError::PartialTransfer(
            "local agent-capture catalog changed during durability verification (since this sync's snapshot); retry cloud sync"
                .to_string(),
        )
    };
    // The schema probe and the re-read share one window; a schema that no
    // longer matches the frozen one is a change.
    let catalog_conn = local_capture_catalog(db_conn).await?;
    let catalog_conn = &catalog_conn;
    let current = super::snapshot::read_in_one_window(catalog_conn, || async move {
        let (session_present, subagent_present) = probe_agent_capture_schema(catalog_conn).await?;
        if !session_present || subagent_present != catalog.subagent_content_present {
            return Ok(None);
        }
        read_agent_capture_catalog(catalog_conn, subagent_present, after_page)
            .await
            .map(Some)
    })
    .await?
    .flatten()
    .ok_or_else(changed)?;
    if &current != catalog {
        return Err(changed());
    }
    ensure_no_unsynced_objects(db_conn, repo_id).await
}

async fn ensure_agent_capture_objects_remote(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &dyn Storage,
    repo_id: &str,
    required_oids: &HashSet<String>,
    artifact_proof: &ReasoningArtifactPublicationProof,
) -> CloudResult<(Vec<ObjectIndexRow>, i64)> {
    let local_map = load_required_local_object_indexes(db_conn, repo_id, required_oids).await?;
    // Local index errors are not repaired by mirroring a bad declaration to D1.
    for proof in artifact_proof.objects.values() {
        if !local_map.get(&proof.oid).is_some_and(|row| {
            row.o_type == "blob"
                && u64::try_from(row.o_size).ok() == Some(proof.byte_len)
                && row.is_synced == 1
        }) {
            return Err(artifact_integrity_failure());
        }
    }
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut hashes = Vec::with_capacity(required.len());
    for oid in &required {
        if !local_map.contains_key(oid.as_str()) {
            return Err(CloudError::PartialTransfer(
                "agent capture requires an object whose local object-index row is missing; run `libra agent doctor --repair`, then retry cloud sync"
                    .to_string(),
            ));
        }
        let bytes = hex::decode(oid).map_err(|_| {
            CloudError::PartialTransfer(
                "agent capture contains an invalid required object id; run `libra agent doctor --repair`, then retry cloud sync"
                    .to_string(),
            )
        })?;
        hashes.push(
            ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &bytes).map_err(
                |_| {
                    CloudError::PartialTransfer(
                        "agent capture contains an invalid required object id; run `libra agent doctor --repair`, then retry cloud sync"
                            .to_string(),
                    )
                },
            )?,
        );
    }

    let remote_rows = d1_client
        .get_object_indexes_by_oids(repo_id, &required)
        .await
        .map_err(|error| cloud_d1_failure("list object indexes for agent capture", &error))?;
    let remote_map = remote_rows
        .iter()
        .map(|row| (row.o_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    // Resolve the repository object store only when an object must be
    // verified; an empty checkpoint projection needs no local storage.
    if !required.is_empty() {
        let local_storage =
            LocalStorage::try_new(path::try_objects().map_err(|error| {
                local_agent_capture_failure("resolve local object store", error)
            })?)
            .map_err(|error| local_agent_capture_failure("open local object store", error))?;
        let verification_rows = required.iter().zip(&hashes).collect::<Vec<_>>();
        for page in agent_capture_object_verification_batches(&verification_rows) {
            // Full content verification remains mandatory, but a fixed-size page
            // overlaps R2 latency without allowing an unbounded fan-out for large
            // histories. Each page completes before the next one starts.
            futures::future::try_join_all(page.iter().map(|(oid, hash)| {
                let local_map = &local_map;
                let remote_map = &remote_map;
                let local_storage = &local_storage;
                async move {
                    let local = local_map.get(oid.as_str()).ok_or_else(|| {
                        CloudError::Generic(
                            "a local object-index row disappeared during cloud sync".to_string(),
                        )
                    })?;
                    let remote_index_matches = remote_map.get(oid.as_str()).is_some_and(|remote| {
                        remote.o_type == local.o_type
                            && remote.o_size == local.o_size
                            && remote.is_synced == 1
                    });
                    if let Some(proof) = artifact_proof.objects.get(oid.as_str()) {
                        publish_checked_agent_capture_object(
                            local_storage,
                            r2_storage,
                            hash,
                            Some(proof),
                        )
                        .await?;
                    } else {
                        publish_validated_agent_capture_object(
                            local_storage,
                            r2_storage,
                            oid,
                            hash,
                        )
                        .await?;
                    }
                    if !remote_index_matches {
                        let object_format = crate::internal::object_format::as_str(
                            git_internal::hash::get_hash_kind(),
                        );
                        d1_client
                            .upsert_object_index_with_format(
                                &local.o_id,
                                &local.o_type,
                                local.o_size,
                                &local.repo_id,
                                local.created_at,
                                Some(object_format),
                            )
                            .await
                            .map_err(|error| {
                                cloud_d1_failure(
                                    "publish required agent-capture object index",
                                    &error,
                                )
                            })?;
                    }
                    Ok::<(), CloudError>(())
                }
            }))
            .await?;
        }
    }
    let verified_rows = d1_client
        .get_object_indexes_by_oids_with_generation(repo_id, &required)
        .await
        .map_err(|error| cloud_d1_failure("verify object indexes for agent capture", &error))?;
    let verified_map = verified_rows
        .0
        .iter()
        .map(|row| (row.o_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    for oid in &required {
        let local = local_map.get(oid.as_str()).ok_or_else(|| {
            CloudError::Generic(
                "a local object-index row disappeared during verification".to_string(),
            )
        })?;
        let valid = verified_map.get(oid.as_str()).is_some_and(|remote| {
            remote.o_type == local.o_type && remote.o_size == local.o_size && remote.is_synced == 1
        });
        if !valid {
            return Err(CloudError::PartialTransfer(
                "a required agent-capture object index is absent or inconsistent in D1; retry `libra cloud sync`"
                    .to_string(),
            ));
        }
    }
    Ok(verified_rows)
}

/// Verify a required capture object before its D1 manifest row can participate
/// in a completed generation. An existence probe is not a content proof: a
/// previous interrupted or corrupted upload may leave bytes under the right
/// key whose hash no longer matches that key. Valid remote payloads avoid a
/// rewrite; missing or corrupt payloads are replaced from validated local data
/// and read back once before publication continues.
pub(super) async fn publish_validated_agent_capture_object(
    local_storage: &LocalStorage,
    r2_storage: &dyn Storage,
    _oid: &str,
    hash: &ObjectHash,
) -> CloudResult<()> {
    publish_checked_agent_capture_object(local_storage, r2_storage, hash, None).await
}

async fn capture_object_for_publication(
    storage: &dyn Storage,
    hash: &ObjectHash,
    artifact: Option<&ReasoningArtifactObjectProof>,
) -> Result<
    (Vec<u8>, git_internal::internal::object::types::ObjectType),
    git_internal::errors::GitError,
> {
    if let Some(proof) = artifact {
        storage.get_typed_bounded(hash, proof.byte_len).await
    } else {
        storage.get(hash).await
    }
}

fn capture_object_matches_proof(
    hash: &ObjectHash,
    bytes: &[u8],
    object_type: git_internal::internal::object::types::ObjectType,
    artifact: Option<&ReasoningArtifactObjectProof>,
) -> bool {
    ObjectHash::from_type_and_data_for_kind(hash.kind(), object_type, bytes).ok() == Some(*hash)
        && artifact.is_none_or(|proof| {
            object_type == git_internal::internal::object::types::ObjectType::Blob
                && bytes.len() as u64 == proof.byte_len
                && hex::encode(Sha256::digest(bytes)) == proof.sha256
                && proof.oid == hash.to_string()
        })
}

async fn publish_checked_agent_capture_object(
    local_storage: &LocalStorage,
    r2_storage: &dyn Storage,
    hash: &ObjectHash,
    artifact: Option<&ReasoningArtifactObjectProof>,
) -> CloudResult<()> {
    if let Ok((remote_bytes, remote_type)) =
        capture_object_for_publication(r2_storage, hash, artifact).await
        && capture_object_matches_proof(hash, &remote_bytes, remote_type, artifact)
    {
        return Ok(());
    }
    let (bytes, object_type) = capture_object_for_publication(local_storage, hash, artifact).await.map_err(|_| {
        CloudError::PartialTransfer(
            "failed to read a required agent-capture object for cloud publication; retry after repairing the local object store"
                .to_string(),
        )
    })?;
    if !capture_object_matches_proof(hash, &bytes, object_type, artifact) {
        return Err(CloudError::PartialTransfer(
            "a required local agent-capture object failed content verification; retry after repairing the local object store"
                .to_string(),
        ));
    }
    r2_storage
        .put(hash, &bytes, object_type)
        .await
        .map_err(|_| {
            CloudError::R2(
                "failed to upload a required agent-capture object; verify cloud storage connectivity and retry"
                    .to_string(),
            )
        })?;
    let (remote_bytes, remote_type) = capture_object_for_publication(r2_storage, hash, artifact).await.map_err(|_| {
        CloudError::R2(
            "failed to read back a required agent-capture object; verify cloud storage connectivity and retry"
                .to_string(),
        )
    })?;
    if !capture_object_matches_proof(hash, &remote_bytes, remote_type, artifact) {
        return Err(CloudError::PartialTransfer(
            "a required remote agent-capture object failed post-upload verification; retry `libra cloud sync`"
                .to_string(),
        ));
    }
    Ok(())
}

async fn load_full_remote_object_manifest(
    d1_client: &D1Client,
    r2_storage: &dyn Storage,
    repo_id: &str,
) -> CloudResult<(Vec<ObjectIndexRow>, i64)> {
    let (rows, generation) = d1_client
        .get_object_indexes_bounded_with_generation(repo_id, AGENT_CAPTURE_MAX_ROWS_PER_TABLE)
        .await
        .map_err(|error| cloud_d1_failure("read retained agent-capture object manifest", &error))?;
    for page in rows.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let mut hashes = Vec::with_capacity(page.len());
        for row in page {
            if row.is_synced != 1 {
                return Err(CloudError::PartialTransfer(
                    "retained remote object is not marked synced; run `libra cloud sync` and retry"
                        .to_string(),
                ));
            }
            let bytes = hex::decode(&row.o_id).map_err(|_| {
                CloudError::PartialTransfer(
                    "retained remote object has an invalid object id; run `libra cloud sync` and retry"
                        .to_string(),
                )
            })?;
            hashes.push(
                ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &bytes)
                    .map_err(|_| {
                        CloudError::PartialTransfer(
                            "retained remote object has an invalid object id; run `libra cloud sync` and retry"
                                .to_string(),
                        )
                    })?,
            );
        }
        let exists = r2_storage.exist_batch(&hashes).await;
        if page.iter().zip(exists).any(|(_, exists)| !exists) {
            return Err(CloudError::PartialTransfer(
                "a retained remote object is absent from remote storage; run `libra cloud sync` and retry"
                    .to_string(),
            ));
        }
    }
    Ok((rows, generation))
}

#[cfg(test)]
pub(super) async fn project_agent_capture_object_indexes(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<Vec<ObjectIndexRow>> {
    let local_map = load_required_local_object_indexes(db_conn, repo_id, required_oids).await?;
    let mut projected = Vec::with_capacity(required_oids.len());
    for oid in required_oids {
        let local = local_map.get(oid.as_str()).ok_or_else(|| {
            CloudError::PartialTransfer(
                "agent capture requires an object whose local object-index row is missing; run `libra agent doctor --repair`, then retry cloud sync"
                    .to_string(),
            )
        })?;
        let row = ObjectIndexRow {
            o_id: local.o_id.clone(),
            o_type: local.o_type.clone(),
            o_size: local.o_size,
            repo_id: local.repo_id.clone(),
            created_at: local.created_at,
            is_synced: 1,
            object_format: None,
        };
        projected.push(row);
    }
    Ok(projected)
}

type SubagentSourceKey = (String, String, String, i64);
type SubagentRevisionKey = (String, String, String, i64, i64);

pub(super) fn claim_key(row: &AgentSubagentContentClaimRow) -> SubagentSourceKey {
    (
        row.parent_session_id.clone(),
        row.provider_kind.clone(),
        row.source_key.clone(),
        row.content_schema_version,
    )
}

fn revision_source_key(row: &AgentSubagentContentRevisionRow) -> SubagentSourceKey {
    (
        row.parent_session_id.clone(),
        row.provider_kind.clone(),
        row.source_key.clone(),
        row.content_schema_version,
    )
}

/// Subagent companion rows selected for one fenced publication.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct PendingSubagentRows {
    pub(super) claims: Vec<AgentSubagentContentClaimRow>,
    pub(super) revisions: Vec<AgentSubagentContentRevisionRow>,
    pub(super) links: Vec<AgentSubagentLinkRow>,
    pub(super) pre_prune_links: Vec<AgentSubagentLinkRow>,
}

impl PendingSubagentRows {
    /// Keep never-mirrored legacy V1 subagent evidence local-only.
    ///
    /// V1 claims and revisions are keyed by an unkeyed source digest, which
    /// must never become a new cloud value (ADR-ACF-03). A source identity the
    /// remote catalog already holds keeps publishing so that copy stays
    /// consistent; any other V1 claim, its revisions, and the association
    /// links of those revisions' content checkpoints are withheld. Content
    /// checkpoints still publish because restore requires every traces commit
    /// to be catalogued. Returns the number of withheld rows.
    pub(super) fn withhold_unmirrored_legacy(
        &mut self,
        local_revisions: &[AgentSubagentContentRevisionRow],
        remote_claims: &[AgentSubagentContentClaimRow],
        remote_revisions: &[AgentSubagentContentRevisionRow],
    ) -> usize {
        let mirrored = remote_claims
            .iter()
            .map(claim_key)
            .chain(remote_revisions.iter().map(revision_source_key))
            .filter(|key| key.3 == SUBAGENT_CONTENT_SCHEMA_VERSION_V1)
            .collect::<HashSet<_>>();
        let local_only = |key: &SubagentSourceKey| {
            key.3 == SUBAGENT_CONTENT_SCHEMA_VERSION_V1 && !mirrored.contains(key)
        };
        let withheld_checkpoints = local_revisions
            .iter()
            .filter(|row| local_only(&revision_source_key(row)))
            .map(|row| row.checkpoint_id.as_str())
            .collect::<HashSet<_>>();
        let before = self.row_count();
        self.claims.retain(|row| !local_only(&claim_key(row)));
        self.revisions
            .retain(|row| !local_only(&revision_source_key(row)));
        for links in [&mut self.links, &mut self.pre_prune_links] {
            links.retain(|row| !withheld_checkpoints.contains(row.content_checkpoint_id.as_str()));
        }
        before.saturating_sub(self.row_count())
    }

    fn row_count(&self) -> usize {
        self.claims
            .len()
            .saturating_add(self.revisions.len())
            .saturating_add(self.links.len())
            .saturating_add(self.pre_prune_links.len())
    }
}

fn revision_key(row: &AgentSubagentContentRevisionRow) -> SubagentRevisionKey {
    (
        row.parent_session_id.clone(),
        row.provider_kind.clone(),
        row.source_key.clone(),
        row.content_schema_version,
        row.revision,
    )
}

fn claim_same_generation(
    left: &AgentSubagentContentClaimRow,
    right: &AgentSubagentContentClaimRow,
) -> bool {
    claim_key(left) == claim_key(right)
        && left.sync_revision == right.sync_revision
        && left.revision_cursor == right.revision_cursor
        && left.current_revision == right.current_revision
        && left.current_checkpoint_id == right.current_checkpoint_id
        && left.current_digest == right.current_digest
}

pub(super) fn should_publish_claim(
    local: &AgentSubagentContentClaimRow,
    remote: Option<&AgentSubagentContentClaimRow>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if claim_same_generation(remote, local) && remote.fence_token >= local.fence_token {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(
            "subagent claim differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.revision_cursor > local.revision_cursor {
        return Err(CloudError::Generic(
            "subagent claim revision high-water would regress; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision > local.sync_revision {
        return Err(CloudError::Generic(
            "subagent claim is older than its recorded remote ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision == local.sync_revision && !claim_same_generation(remote, local) {
        return Err(CloudError::Generic(
            "subagent claim conflicts with the remote at the same sync generation".to_string(),
        ));
    }
    Ok(remote.sync_revision < local.sync_revision || remote.fence_token < local.fence_token)
}

pub(super) fn should_publish_session(
    local: &AgentSessionV2Row,
    remote: Option<&AgentSessionV2Row>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote == local {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(
            "agent session differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision < local.sync_revision {
        return Ok(true);
    }
    Err(CloudError::Generic(
        "agent session does not descend monotonically from its recorded remote ancestor"
            .to_string(),
    ))
}

pub(super) fn remote_catalog_is_legacy_generation_zero_bootstrap(
    has_remote_generation: bool,
    local_cloud_base: Option<i64>,
    rows: &AgentCaptureRestoreCatalogRows,
) -> bool {
    !has_remote_generation
        && local_cloud_base.is_none()
        && (!rows.sessions.is_empty() || !rows.checkpoints.is_empty())
        && rows.sessions.iter().all(|row| row.sync_revision == 0)
        && rows.checkpoints.iter().all(|row| row.sync_revision == 0)
        && rows.prune_tombstones.is_empty()
        && rows.claims.is_empty()
        && rows.revisions.is_empty()
        && rows.links.is_empty()
}

pub(super) fn remote_generation_is_known_ancestor(
    remote: Option<&AgentCaptureGenerationRow>,
    local_cloud_base: Option<i64>,
) -> bool {
    remote.is_some_and(|generation| match generation.state.as_str() {
        "complete" => local_cloud_base == Some(generation.generation),
        // A publishing generation is the immediate child of the last
        // completed base observed by its writer. Allow preflight to reconcile
        // that staged catalog so the server-side lease/CAS can eventually
        // resume an abandoned publication. This does not let an active writer
        // be displaced: begin_agent_capture_generation_from still enforces the
        // five-minute server-timestamped lease before issuing a new token.
        "publishing" => match local_cloud_base {
            Some(base) => base.checked_add(1) == Some(generation.generation),
            None => generation.generation == 1,
        },
        _ => false,
    })
}

pub(super) fn should_publish_link(
    local: &AgentSubagentLinkRow,
    remote: Option<&AgentSubagentLinkRow>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote == local {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(
            "subagent link differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision < local.sync_revision {
        return Ok(true);
    }
    Err(CloudError::Generic(
        "subagent link does not descend monotonically from its recorded remote ancestor"
            .to_string(),
    ))
}

pub(super) fn checkpoint_rewrite_compatible(
    left: &AgentCheckpointV2Row,
    right: &AgentCheckpointV2Row,
) -> bool {
    left.checkpoint_id == right.checkpoint_id
        && left.session_id == right.session_id
        && left.parent_checkpoint_id == right.parent_checkpoint_id
        && left.scope == right.scope
        && left.parent_commit == right.parent_commit
        && left.tool_use_id == right.tool_use_id
        && left.subagent_session_id == right.subagent_session_id
        && left.description == right.description
        && left.created_at == right.created_at
}

pub(super) fn should_publish_checkpoint(
    local: &AgentCheckpointV2Row,
    remote: Option<&AgentCheckpointV2Row>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote.sync_revision == local.sync_revision {
        if remote == local {
            return Ok(false);
        }
        return Err(CloudError::Generic(
            "agent checkpoint diverges from the remote at the same sync generation".to_string(),
        ));
    }
    if !checkpoint_rewrite_compatible(local, remote) {
        return Err(CloudError::Generic(
            "agent checkpoint conflicts with the remote immutable identity".to_string(),
        ));
    }
    if local.sync_revision > remote.sync_revision && !remote_is_known_ancestor {
        return Err(CloudError::Generic(
            "agent checkpoint differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    Ok(local.sync_revision > remote.sync_revision)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompanionValidationMode {
    Complete,
    Publishing,
}

const SUBAGENT_CONTENT_SCHEMA_VERSION_V1: i64 = 1;
const SUBAGENT_SOURCE_KEY_V1_PREFIX: &str = "source/sha256/";
const SUBAGENT_SOURCE_KEY_V2_PREFIX: &str = "source/subagent-hmac-v2/";
const IMPORT_SOURCE_SCHEMA_VERSION_V1: i64 = 1;

/// Validate the only source-key forms that may cross the cloud companion
/// boundary. V1 is immutable historical evidence; new V2 rows must carry the
/// repository-keyed commitment, never a locator or an unkeyed digest.
fn validate_subagent_companion_source_key(
    content_schema_version: i64,
    source_key: &str,
    side: &str,
) -> CloudResult<()> {
    let prefix = match content_schema_version {
        SUBAGENT_CONTENT_SCHEMA_VERSION_V1 => SUBAGENT_SOURCE_KEY_V1_PREFIX,
        crate::internal::ai::subagent_content::SUBAGENT_CONTENT_SCHEMA_VERSION => {
            SUBAGENT_SOURCE_KEY_V2_PREFIX
        }
        _ => {
            return Err(CloudError::Generic(format!(
                "{side} subagent companion uses an unsupported content schema"
            )));
        }
    };
    let valid = source_key.strip_prefix(prefix).is_some_and(|commitment| {
        commitment.len() == 64
            && commitment
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    });
    if valid {
        Ok(())
    } else {
        // The source key can be a raw provider-relative path in a malicious
        // remote row. Keep diagnostics content-free so validation cannot
        // become another locator disclosure sink.
        Err(CloudError::Generic(format!(
            "{side} subagent companion has an invalid source commitment"
        )))
    }
}

pub(super) fn validate_agent_capture_companions(
    checkpoints: &[AgentCheckpointV2Row],
    claims: &[AgentSubagentContentClaimRow],
    revisions: &[AgentSubagentContentRevisionRow],
    links: &[AgentSubagentLinkRow],
    side: &str,
    mode: CompanionValidationMode,
) -> CloudResult<()> {
    for claim in claims {
        validate_subagent_companion_source_key(
            claim.content_schema_version,
            &claim.source_key,
            side,
        )?;
    }
    for revision in revisions {
        validate_subagent_companion_source_key(
            revision.content_schema_version,
            &revision.source_key,
            side,
        )?;
    }
    let checkpoint_map: HashMap<&str, &AgentCheckpointV2Row> = checkpoints
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect();
    let claim_keys: HashSet<SubagentSourceKey> = claims.iter().map(claim_key).collect();
    let revision_map: HashMap<SubagentRevisionKey, &AgentSubagentContentRevisionRow> = revisions
        .iter()
        .map(|row| (revision_key(row), row))
        .collect();
    let link_map: HashMap<&str, &AgentSubagentLinkRow> = links
        .iter()
        .map(|row| (row.content_checkpoint_id.as_str(), row))
        .collect();
    let revision_checkpoint_ids: HashSet<&str> = revisions
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect();
    for revision in revisions {
        let source_key = (
            revision.parent_session_id.clone(),
            revision.provider_kind.clone(),
            revision.source_key.clone(),
            revision.content_schema_version,
        );
        if !claim_keys.contains(&source_key) && mode == CompanionValidationMode::Complete {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision has no source claim dependency"
            )));
        }
        let Some(checkpoint) = checkpoint_map.get(revision.checkpoint_id.as_str()) else {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision has no checkpoint dependency"
            )));
        };
        if checkpoint.session_id != revision.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision disagrees with its checkpoint parent"
            )));
        }
        if checkpoint.scope != "subagent" {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision references a non-subagent checkpoint"
            )));
        }
        if mode == CompanionValidationMode::Complete {
            let link = link_map
                .get(revision.checkpoint_id.as_str())
                .ok_or_else(|| {
                    CloudError::Generic(format!(
                        "{side} subagent revision has no association link dependency"
                    ))
                })?;
            if link.parent_session_id != revision.parent_session_id {
                return Err(CloudError::Generic(format!(
                    "{side} subagent revision disagrees with its association parent"
                )));
            }
        }
        if mode == CompanionValidationMode::Complete
            && let Some(claim) = claims.iter().find(|claim| claim_key(claim) == source_key)
            && revision.revision > claim.revision_cursor
        {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision is newer than its completed source claim"
            )));
        }
    }
    for link in links {
        let Some(checkpoint) = checkpoint_map.get(link.content_checkpoint_id.as_str()) else {
            return Err(CloudError::Generic(format!(
                "{side} subagent link has no checkpoint dependency"
            )));
        };
        if checkpoint.session_id != link.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} subagent link disagrees with its checkpoint parent"
            )));
        }
        if checkpoint.scope != "subagent" {
            return Err(CloudError::Generic(format!(
                "{side} subagent link references a non-subagent content checkpoint"
            )));
        }
        if mode == CompanionValidationMode::Complete
            && !revision_checkpoint_ids.contains(link.content_checkpoint_id.as_str())
        {
            return Err(CloudError::Generic(format!(
                "{side} subagent link has no immutable revision dependency"
            )));
        }
        if let Some(boundary) = link.boundary_checkpoint_id.as_deref() {
            let boundary_checkpoint = checkpoint_map.get(boundary).ok_or_else(|| {
                CloudError::Generic(format!(
                    "{side} resolved subagent link has no boundary checkpoint dependency"
                ))
            })?;
            if boundary_checkpoint.scope != "subagent"
                || boundary_checkpoint.session_id != link.parent_session_id
                || revision_checkpoint_ids.contains(boundary)
            {
                return Err(CloudError::Generic(format!(
                    "{side} resolved subagent link references an invalid boundary checkpoint"
                )));
            }
        }
    }
    for claim in claims {
        if claim.revision_cursor < claim.current_revision {
            return Err(CloudError::Generic(format!(
                "{side} subagent claim cursor is behind its current revision"
            )));
        }
        if claim.current_revision == 0 {
            if claim.current_checkpoint_id.is_some() || claim.current_digest.is_some() {
                return Err(CloudError::Generic(format!(
                    "{side} zero-revision subagent claim has a materialized current leaf"
                )));
            }
            continue;
        }
        let checkpoint_id = claim.current_checkpoint_id.as_deref().ok_or_else(|| {
            CloudError::Generic(format!("{side} current subagent claim has no checkpoint"))
        })?;
        let digest = claim.current_digest.as_deref().ok_or_else(|| {
            CloudError::Generic(format!("{side} current subagent claim has no digest"))
        })?;
        let revision = revision_map
            .get(&(
                claim.parent_session_id.clone(),
                claim.provider_kind.clone(),
                claim.source_key.clone(),
                claim.content_schema_version,
                claim.current_revision,
            ))
            .ok_or_else(|| {
                CloudError::Generic(format!(
                    "{side} current subagent claim has no immutable revision dependency"
                ))
            })?;
        if revision.checkpoint_id != checkpoint_id || revision.content_digest != digest {
            return Err(CloudError::Generic(format!(
                "{side} current subagent claim disagrees with its immutable revision"
            )));
        }
        let link = link_map.get(checkpoint_id).ok_or_else(|| {
            CloudError::Generic(format!(
                "{side} current subagent claim has no association link dependency"
            ))
        })?;
        if link.parent_session_id != claim.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} current subagent claim disagrees with its association parent"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_agent_capture_session_dependencies(
    sessions: &[AgentSessionV2Row],
    checkpoints: &[AgentCheckpointV2Row],
    claims: &[AgentSubagentContentClaimRow],
    side: &str,
) -> CloudResult<()> {
    let session_ids = sessions
        .iter()
        .map(|row| row.session_id.as_str())
        .collect::<HashSet<_>>();
    for checkpoint in checkpoints {
        if !session_ids.contains(checkpoint.session_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "{side} checkpoint has no session dependency"
            )));
        }
    }
    for claim in claims {
        if !session_ids.contains(claim.parent_session_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "{side} subagent claim has no parent session dependency"
            )));
        }
    }
    Ok(())
}

/// Import-ownership members of a session's `metadata_json`. Every member of
/// this closed family is ownership-bearing: even one pre-versioned field can
/// carry a provider-root-relative source locator or an unkeyed identity.
const IMPORT_OWNERSHIP_METADATA_FIELDS: &[&str] = &[
    "repository_identity",
    "source_kind",
    "source_id",
    "source_fingerprint",
    "import_source_schema_version",
    "import_provisional",
    "imported",
    "transcript_snapshot",
];

/// Project a session row onto the cloud boundary (sync and restore alike).
///
/// Live rows and complete V2 import records cross byte-identically; the
/// catalog's closed parser validates V2 so cloud never accepts a shape the
/// catalog would not persist. Legacy (unversioned or V1) ownership is
/// immutable local evidence whose locator and unkeyed digests must never
/// become a new cloud or restored value, so every ownership member is
/// omitted and the remaining metadata crosses unchanged. Duplicate keys,
/// non-object metadata, unknown schemas and invalid V2 records stay fixed,
/// content-free errors.
pub(super) fn project_agent_session_for_cloud(
    row: &AgentSessionV2Row,
    side: &str,
) -> CloudResult<AgentSessionV2Row> {
    let malformed = || {
        CloudError::Generic(format!(
            "{side} agent session has malformed import ownership metadata"
        ))
    };
    let unsupported = || {
        CloudError::Generic(format!(
            "{side} agent session uses an unsupported import ownership schema"
        ))
    };
    crate::internal::ai::capture::catalog::validate_json_object_keys_are_unique(&row.metadata_json)
        .map_err(|_| malformed())?;
    let serde_json::Value::Object(mut metadata) =
        serde_json::from_str(&row.metadata_json).map_err(|_| malformed())?
    else {
        return Err(malformed());
    };
    if !IMPORT_OWNERSHIP_METADATA_FIELDS
        .iter()
        .any(|field| metadata.contains_key(*field))
    {
        return Ok(row.clone());
    }
    let legacy = match metadata.get("import_source_schema_version") {
        None => true,
        Some(value) => match value.as_i64().ok_or_else(unsupported)? {
            IMPORT_SOURCE_SCHEMA_VERSION_V1 => true,
            crate::internal::ai::agent_import::IMPORT_IDENTITY_SCHEMA_VERSION_V2 => false,
            _ => return Err(unsupported()),
        },
    };
    if !legacy {
        crate::internal::ai::capture::catalog::validate_v2_import_session_record(
            &row.metadata_json,
            &row.redaction_report,
        )
        .map_err(|_| {
            CloudError::Generic(format!(
                "{side} agent session has invalid V2 import ownership metadata"
            ))
        })?;
        return Ok(row.clone());
    }
    for field in IMPORT_OWNERSHIP_METADATA_FIELDS {
        metadata.remove(*field);
    }
    let metadata_json =
        serde_json::to_string(&serde_json::Value::Object(metadata)).map_err(|_| malformed())?;
    Ok(AgentSessionV2Row {
        metadata_json,
        ..row.clone()
    })
}

pub(super) fn object_manifest_scope_for_remote_catalog(
    local: &[AgentCheckpointV2Row],
    effective: &[AgentCheckpointV2Row],
) -> AgentCaptureObjectManifestScope {
    let local_rows = local
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    if effective
        .iter()
        .any(|row| local_rows.get(row.checkpoint_id.as_str()).copied() != Some(row))
    {
        AgentCaptureObjectManifestScope::FullRemoteIndex
    } else {
        AgentCaptureObjectManifestScope::CheckpointProjection
    }
}

pub(super) fn build_effective_checkpoint_catalog(
    local: &[AgentCheckpointV2Row],
    remote: &[AgentCheckpointV2Row],
    local_tombstones: &[AgentCheckpointPruneTombstoneRow],
    remote_tombstones: &[AgentCheckpointPruneTombstoneRow],
    remote_is_known_ancestor: bool,
) -> CloudResult<(Vec<AgentCheckpointV2Row>, Vec<AgentCheckpointV2Row>)> {
    let local_map = local
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let remote_map = remote
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let local_tombstone_ids = local_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();
    let remote_tombstone_ids = remote_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();

    if local
        .iter()
        .any(|row| remote_tombstone_ids.contains(row.checkpoint_id.as_str()))
    {
        return Err(CloudError::Generic(
            "an agent checkpoint was already pruned by another cloud writer; restore the current cloud snapshot before syncing this stale clone"
                .to_string(),
        ));
    }

    let mut pending = Vec::new();
    let mut effective = Vec::new();
    for remote_row in remote {
        if local_tombstone_ids.contains(remote_row.checkpoint_id.as_str()) {
            continue;
        }
        let Some(local_row) = local_map.get(remote_row.checkpoint_id.as_str()).copied() else {
            return Err(CloudError::Generic(
                "remote capture contains a checkpoint absent from this local catalog without an ordinary-prune tombstone; restore the current cloud capture or retry after reconciling the local checkpoint catalog before publishing a new generation".to_string(),
            ));
        };
        if remote_row.sync_revision > local_row.sync_revision {
            return Err(CloudError::Generic(
                "remote checkpoint is newer than this clone's traces history; restore the current cloud snapshot before syncing"
                    .to_string(),
            ));
        }
        if should_publish_checkpoint(local_row, Some(remote_row), remote_is_known_ancestor)? {
            pending.push(local_row.clone());
            effective.push(local_row.clone());
        } else {
            effective.push(remote_row.clone());
        }
    }
    for local_row in local {
        if !remote_map.contains_key(local_row.checkpoint_id.as_str()) {
            pending.push(local_row.clone());
            effective.push(local_row.clone());
        }
    }
    effective.sort_by(|left, right| left.checkpoint_id.cmp(&right.checkpoint_id));
    Ok((pending, effective))
}

fn checkpoint_catalog_matches(
    left: &[AgentCheckpointV2Row],
    right: &[AgentCheckpointV2Row],
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let right_map = right
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    left.iter()
        .all(|row| right_map.get(row.checkpoint_id.as_str()).copied() == Some(row))
}

/// Freeze the live catalog and publish it (tests).
#[cfg(test)]
pub(super) async fn sync_agent_capture_tables(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<AgentCaptureSyncOutcome> {
    let catalog_conn = local_capture_catalog(db_conn).await?;
    let frozen = freeze_agent_capture(&catalog_conn, None).await;
    sync_frozen_agent_capture_tables(db_conn, d1_client, r2_storage, repo_id, progress, &frozen)
        .await
}

/// Mirror one coherent local agent-capture/object generation to D1: the
/// catalog frozen with this sync's snapshot (PI-26), once every publication
/// check has passed on the current database. Remote state is paged and used
/// as an incremental high-water mark; only missing or strictly newer rows
/// are sent, in bounded multi-row requests. Immutable conflicts fail before
/// publication. Dependencies publish first and claims publish last.
pub(super) async fn sync_frozen_agent_capture_tables(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
    frozen: &FrozenAgentCapture,
) -> CloudResult<AgentCaptureSyncOutcome> {
    tokio::time::timeout(
        AGENT_CAPTURE_CLOUD_DEADLINE,
        sync_agent_capture_tables_inner(db_conn, d1_client, r2_storage, repo_id, progress, frozen),
    )
    .await
    .map_err(|_| {
        CloudError::PartialTransfer(
            "agent capture cloud sync exceeded its 120-second deadline; retry the operation"
                .to_string(),
        )
    })?
}

async fn sync_agent_capture_tables_inner(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &dyn Storage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
    frozen: &FrozenAgentCapture,
) -> CloudResult<AgentCaptureSyncOutcome> {
    let lifecycle_deadline = std::time::Instant::now()
        .checked_add(AGENT_CAPTURE_CLOUD_DEADLINE)
        .ok_or_else(artifact_integrity_failure)?;

    if matches!(frozen, FrozenAgentCapture::LegacySchema) {
        return Ok(AgentCaptureSyncOutcome::SkippedLegacySchema);
    }
    progress.on_agent_capture_starting();
    let catalog = match frozen {
        FrozenAgentCapture::Catalog(catalog) => catalog,
        FrozenAgentCapture::Unreadable { reason } => {
            return Err(CloudError::PartialTransfer(reason.clone()));
        }
        FrozenAgentCapture::LegacySchema => {
            return Ok(AgentCaptureSyncOutcome::SkippedLegacySchema);
        }
    };
    let subagent_content_present = catalog.subagent_content_present;
    let snapshot = verify_frozen_agent_capture(catalog, db_conn, repo_id).await?;

    d1_client
        .ensure_agent_session_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure agent-session table", &error))?;
    d1_client
        .ensure_agent_checkpoint_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure agent-checkpoint table", &error))?;
    d1_client
        .ensure_agent_capture_generation_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure agent-capture generation table", &error))?;
    d1_client
        .ensure_agent_checkpoint_prune_tombstone_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure checkpoint prune tombstones", &error))?;
    d1_client
        .ensure_agent_import_tombstone_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure agent import tombstones", &error))?;
    if subagent_content_present {
        d1_client
            .ensure_agent_subagent_content_tables()
            .await
            .map_err(|error| cloud_d1_failure("ensure subagent-content tables", &error))?;
    }

    let remote_generation = d1_client
        .get_agent_capture_generation(repo_id)
        .await
        .map_err(|error| cloud_d1_failure("read agent-capture generation before sync", &error))?;
    let catalog_conn = local_capture_catalog(db_conn).await?;
    let local_cloud_base = load_local_agent_capture_cloud_base(&catalog_conn, repo_id).await?;
    let remote_catalog = d1_client
        .list_agent_capture_restore_catalog_rows(
            repo_id,
            subagent_content_present,
            AGENT_CAPTURE_RESTORE_MAX_ROWS,
        )
        .await
        .map_err(|error| cloud_d1_failure("list agent-capture catalog before sync", &error))?;
    // The one-time legacy adoption copies only session/checkpoint rows at
    // generation zero and deliberately has no completed generation manifest.
    // Treat that exact projection as the bootstrap ancestor so the first
    // current client can replace revision zero under its first fenced
    // generation. Any current-only row type or nonzero revision fails closed.
    let remote_is_known_ancestor =
        remote_generation_is_known_ancestor(remote_generation.as_ref(), local_cloud_base)
            || remote_catalog_is_legacy_generation_zero_bootstrap(
                remote_generation.is_some(),
                local_cloud_base,
                &remote_catalog,
            );
    let AgentCaptureRestoreCatalogRows {
        sessions: remote_sessions,
        checkpoints: remote_checkpoints,
        prune_tombstones: remote_prune_tombstones,
        import_tombstones: remote_import_tombstones,
        claims: remote_claims,
        revisions: remote_revisions,
        links: remote_links,
        remaining_rows: _,
    } = remote_catalog;
    // PD-03: the effective tombstone set is the UNION of local and remote
    // (newest erased_at per provider identity), and every remote catalog
    // row belonging to an erased session id is dropped BEFORE conflict/
    // effective-set computation — the publish cascade deletes the same
    // rows remotely, so the post-publish verification stays coherent.
    let merged_import_tombstones =
        merge_import_tombstones(&snapshot.import_tombstones, &remote_import_tombstones);
    let erased_session_ids: HashSet<&str> = merged_import_tombstones
        .iter()
        .map(|row| row.erased_session_id.as_str())
        .collect();
    let remote_sessions: Vec<AgentSessionV2Row> = remote_sessions
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .collect();
    let erased_checkpoint_ids: HashSet<&str> = remote_checkpoints
        .iter()
        .filter(|row| erased_session_ids.contains(row.session_id.as_str()))
        .map(|row| row.checkpoint_id.as_str())
        .collect();
    let remote_checkpoints: Vec<AgentCheckpointV2Row> = remote_checkpoints
        .iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .cloned()
        .collect();
    let remote_claims: Vec<AgentSubagentContentClaimRow> = remote_claims
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.parent_session_id.as_str()))
        .collect();
    let remote_revisions: Vec<AgentSubagentContentRevisionRow> = remote_revisions
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.checkpoint_id.as_str()))
        .collect();
    let remote_links: Vec<AgentSubagentLinkRow> = remote_links
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.content_checkpoint_id.as_str()))
        .collect();
    // Earlier releases mirrored legacy ownership verbatim. Compare projected
    // rows so an already mirrored legacy session is not republished, while a
    // newer local row replaces it only with its projection.
    let remote_sessions = remote_sessions
        .iter()
        .map(|row| project_agent_session_for_cloud(row, "remote"))
        .collect::<CloudResult<Vec<_>>>()?;
    validate_agent_capture_companions(
        &remote_checkpoints,
        &remote_claims,
        &remote_revisions,
        &remote_links,
        "remote",
        CompanionValidationMode::Publishing,
    )?;
    validate_agent_capture_session_dependencies(
        &remote_sessions,
        &remote_checkpoints,
        &remote_claims,
        "remote",
    )?;
    let remote_session_map: HashMap<&str, &AgentSessionV2Row> = remote_sessions
        .iter()
        .map(|row| (row.session_id.as_str(), row))
        .collect();
    let mut pending_sessions = Vec::new();
    for row in &snapshot.sessions {
        if should_publish_session(
            row,
            remote_session_map.get(row.session_id.as_str()).copied(),
            remote_is_known_ancestor,
        )? {
            pending_sessions.push(row.clone());
        }
    }
    let (pending_checkpoints, effective_checkpoints) = build_effective_checkpoint_catalog(
        &snapshot.checkpoints,
        &remote_checkpoints,
        &snapshot.prune_tombstones,
        &remote_prune_tombstones,
        remote_is_known_ancestor,
    )?;

    let object_manifest_scope =
        object_manifest_scope_for_remote_catalog(&snapshot.checkpoints, &effective_checkpoints);
    let artifact_proof = if effective_checkpoints.is_empty() {
        ReasoningArtifactPublicationProof {
            checkpoints: Vec::new(),
            objects: HashMap::new(),
        }
    } else {
        let storage = util::try_get_storage_path(None)
            .map_err(|error| local_agent_capture_failure("resolve artifact object store", error))?;
        ReasoningArtifactPublicationProof::establish(
            &storage,
            &effective_checkpoints,
            lifecycle_deadline,
        )
        .await?
    };
    let (required_object_indexes, required_object_generation) =
        ensure_agent_capture_objects_remote(
            db_conn,
            d1_client,
            r2_storage,
            repo_id,
            &snapshot.required_oids,
            &artifact_proof,
        )
        .await?;
    let (object_manifest_rows, object_index_generation) = match object_manifest_scope {
        AgentCaptureObjectManifestScope::CheckpointProjection => {
            (required_object_indexes, required_object_generation)
        }
        AgentCaptureObjectManifestScope::FullRemoteIndex => {
            load_full_remote_object_manifest(d1_client, r2_storage, repo_id).await?
        }
    };
    validate_agent_capture_restore_row_budget(&snapshot, object_manifest_rows.len())?;
    validate_checkpoint_object_index_roots(
        &effective_checkpoints,
        &object_manifest_rows,
        "projected remote",
    )?;
    artifact_proof.validate_indexes(&object_manifest_rows)?;
    let (object_index_digest, object_index_count) =
        agent_capture_object_index_digest(&object_manifest_rows)?;

    let remote_revision_map: HashMap<SubagentRevisionKey, &AgentSubagentContentRevisionRow> =
        remote_revisions
            .iter()
            .map(|row| (revision_key(row), row))
            .collect();
    let mut pending_revisions = Vec::new();
    for row in &snapshot.revisions {
        match remote_revision_map.get(&revision_key(row)) {
            None => pending_revisions.push(row.clone()),
            Some(remote) if *remote == row => {}
            Some(_) => {
                return Err(CloudError::Generic(
                    "an immutable subagent revision conflicts with the remote; restore the current cloud snapshot before syncing"
                        .to_string(),
                ));
            }
        }
    }

    let remote_link_map: HashMap<&str, &AgentSubagentLinkRow> = remote_links
        .iter()
        .map(|row| (row.content_checkpoint_id.as_str(), row))
        .collect();
    let mut pending_links = Vec::new();
    for row in &snapshot.links {
        if should_publish_link(
            row,
            remote_link_map
                .get(row.content_checkpoint_id.as_str())
                .copied(),
            remote_is_known_ancestor,
        )? {
            pending_links.push(row.clone());
        }
    }
    let prune_ids = snapshot
        .prune_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();
    let (pre_prune_links, pending_links): (Vec<_>, Vec<_>) =
        pending_links.into_iter().partition(|row| {
            !prune_ids.contains(row.content_checkpoint_id.as_str())
                && row.boundary_checkpoint_id.is_none()
                && remote_link_map
                    .get(row.content_checkpoint_id.as_str())
                    .and_then(|remote| remote.boundary_checkpoint_id.as_deref())
                    .is_some_and(|boundary| prune_ids.contains(boundary))
        });
    if remote_links.iter().any(|remote| {
        !prune_ids.contains(remote.content_checkpoint_id.as_str())
            && remote
                .boundary_checkpoint_id
                .as_deref()
                .is_some_and(|boundary| prune_ids.contains(boundary))
            && !pre_prune_links
                .iter()
                .any(|local| local.content_checkpoint_id == remote.content_checkpoint_id)
    }) {
        return Err(CloudError::Generic(
            "a remote subagent link still resolves through a checkpoint being pruned, but this clone has no newer unresolved link generation; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }

    let remote_claim_map: HashMap<SubagentSourceKey, &AgentSubagentContentClaimRow> = remote_claims
        .iter()
        .map(|row| (claim_key(row), row))
        .collect();
    let mut pending_claims = Vec::new();
    for row in &snapshot.claims {
        if should_publish_claim(
            row,
            remote_claim_map.get(&claim_key(row)).copied(),
            remote_is_known_ancestor,
        )? {
            pending_claims.push(row.clone());
        }
    }
    let mut pending_subagent = PendingSubagentRows {
        claims: pending_claims,
        revisions: pending_revisions,
        links: pending_links,
        pre_prune_links,
    };
    let withheld_legacy_rows = pending_subagent.withhold_unmirrored_legacy(
        &snapshot.revisions,
        &remote_claims,
        &remote_revisions,
    );
    let PendingSubagentRows {
        claims: pending_claims,
        revisions: pending_revisions,
        links: pending_links,
        pre_prune_links,
    } = pending_subagent;

    // The catalog is still the frozen one and nothing unsynced appeared,
    // checked once more right before the first remote catalog mutation.
    recheck_frozen_agent_capture(catalog, db_conn, repo_id, None).await?;

    // All remote conflict and object-durability checks happen before this
    // transition. A transient preflight failure therefore leaves the last
    // complete manifest restorable instead of needlessly wedging it in
    // `publishing` before any fenced capture-catalog mutation.
    let publish_token = Uuid::new_v4().to_string();
    d1_client
        .begin_agent_capture_generation_from(
            repo_id,
            &publish_token,
            remote_generation
                .as_ref()
                .map(|generation| generation.generation),
            AgentCaptureGenerationManifest {
                object_index_digest: &object_index_digest,
                object_index_count,
                object_index_scope: object_manifest_scope.as_str(),
                object_index_generation,
                traces_head: snapshot.traces_head.as_deref(),
            },
        )
        .await
        .map_err(|error| cloud_d1_failure("begin agent-capture generation", &error))?;
    for rows in agent_capture_batches(&pending_sessions) {
        d1_client
            .sync_agent_sessions_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| agent_capture_row_failure("sync agent-session batch", &error))?;
    }
    // Boundary associations must become unresolved before their boundary
    // checkpoint is deleted. This preserves a Publishing-valid graph at every
    // request boundary; new content links still publish after checkpoints.
    for rows in agent_capture_batches(&pre_prune_links) {
        d1_client
            .sync_agent_subagent_links_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| cloud_d1_failure("sync pre-prune subagent-link batch", &error))?;
    }
    for rows in agent_capture_batches(&snapshot.prune_tombstones) {
        d1_client
            .sync_agent_checkpoint_prune_tombstones_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| cloud_d1_failure("sync checkpoint prune tombstones", &error))?;
    }
    for rows in agent_capture_batches(&pending_checkpoints) {
        d1_client
            .sync_agent_checkpoints_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| agent_capture_row_failure("sync agent-checkpoint batch", &error))?;
    }
    for rows in agent_capture_batches(&pending_revisions) {
        d1_client
            .sync_agent_subagent_revisions_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| agent_capture_row_failure("sync subagent-revision batch", &error))?;
    }
    for rows in agent_capture_batches(&pending_links) {
        d1_client
            .sync_agent_subagent_links_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| agent_capture_row_failure("sync subagent-link batch", &error))?;
    }
    for rows in agent_capture_batches(&pending_claims) {
        d1_client
            .sync_agent_subagent_claims_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| agent_capture_row_failure("sync subagent-claim batch", &error))?;
    }
    // PD-03: the session tombstones publish LAST so their cascade delete
    // is final regardless of what earlier batches upserted.
    for rows in agent_capture_batches(&merged_import_tombstones) {
        d1_client
            .sync_agent_import_tombstones_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| cloud_d1_failure("sync agent import tombstones", &error))?;
    }

    let AgentCaptureRestoreCatalogRows {
        sessions: completed_sessions,
        checkpoints: completed_checkpoints,
        prune_tombstones: _,
        import_tombstones: _,
        claims: completed_claims,
        revisions: completed_revisions,
        links: completed_links,
        remaining_rows: completed_remaining_rows,
    } = d1_client
        .list_agent_capture_restore_catalog_rows(
            repo_id,
            subagent_content_present,
            AGENT_CAPTURE_RESTORE_MAX_ROWS,
        )
        .await
        .map_err(|error| cloud_d1_failure("verify agent-capture catalog", &error))?;
    if !checkpoint_catalog_matches(&completed_checkpoints, &effective_checkpoints) {
        return Err(CloudError::PartialTransfer(
            "remote checkpoint catalog changed during agent-capture publication; retry cloud sync"
                .to_string(),
        ));
    }
    for session in &completed_sessions {
        project_agent_session_for_cloud(session, "completed remote")?;
    }
    validate_agent_capture_companions(
        &completed_checkpoints,
        &completed_claims,
        &completed_revisions,
        &completed_links,
        "completed remote",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        &completed_sessions,
        &completed_checkpoints,
        &completed_claims,
        "completed remote",
    )?;
    let completed_scope =
        object_manifest_scope_for_remote_catalog(&snapshot.checkpoints, &effective_checkpoints);
    if completed_scope != object_manifest_scope {
        return Err(CloudError::PartialTransfer(
            "remote checkpoint catalog changed its object-manifest scope during publication; retry cloud sync"
                .to_string(),
        ));
    }
    let mut required_oids = snapshot.required_oids.iter().cloned().collect::<Vec<_>>();
    required_oids.sort();
    let (completed_object_indexes, completed_object_generation) = match object_manifest_scope {
        AgentCaptureObjectManifestScope::CheckpointProjection => {
            if required_oids.len() > completed_remaining_rows {
                return Err(CloudError::PartialTransfer(format!(
                    "completed remote agent-capture verification exceeds its aggregate {}-row safety bound before reading object indexes",
                    AGENT_CAPTURE_RESTORE_MAX_ROWS
                )));
            }
            d1_client
                .get_object_indexes_by_oids_with_generation(repo_id, &required_oids)
                .await
                .map_err(|error| {
                    cloud_d1_failure("verify fenced agent-capture object indexes", &error)
                })?
        }
        AgentCaptureObjectManifestScope::FullRemoteIndex => d1_client
            .get_object_indexes_bounded_with_generation(repo_id, completed_remaining_rows)
            .await
            .map_err(|error| {
                cloud_d1_failure("verify retained agent-capture object manifest", &error)
            })?,
    };
    completed_remaining_rows
        .checked_sub(completed_object_indexes.len())
        .ok_or_else(|| {
            CloudError::PartialTransfer(format!(
                "completed remote agent-capture verification exceeds its aggregate {}-row safety bound while reading object indexes",
                AGENT_CAPTURE_RESTORE_MAX_ROWS
            ))
        })?;
    validate_checkpoint_object_index_roots(
        &completed_checkpoints,
        &completed_object_indexes,
        "completed remote",
    )?;
    let completed_object_manifest = agent_capture_object_index_digest(&completed_object_indexes)?;
    artifact_proof.validate_catalog(&completed_checkpoints)?;
    artifact_proof.validate_indexes(&completed_object_indexes)?;
    if completed_object_manifest != (object_index_digest.clone(), object_index_count)
        || completed_object_generation != object_index_generation
    {
        return Err(CloudError::PartialTransfer(
            "remote object indexes changed during agent-capture publication; retry cloud sync"
                .to_string(),
        ));
    }
    let completed_generation = d1_client
        .complete_agent_capture_generation(repo_id, &publish_token, object_index_generation)
        .await
        .map_err(|error| cloud_d1_failure("complete agent-capture generation", &error))?;
    store_local_agent_capture_cloud_base(&catalog_conn, repo_id, completed_generation.generation)
        .await?;
    if withheld_legacy_rows > 0 {
        progress.on_agent_capture_warning(AGENT_CAPTURE_LEGACY_LOCAL_ONLY_MESSAGE);
    }

    let sessions_synced = pending_sessions.len();
    let checkpoints_synced = pending_checkpoints.len();
    let subagent_rows_synced = pending_claims
        .len()
        .saturating_add(pending_revisions.len())
        .saturating_add(pending_links.len())
        .saturating_add(pre_prune_links.len());
    progress.on_agent_capture_done_with_subagents(
        sessions_synced,
        0,
        checkpoints_synced,
        0,
        subagent_rows_synced,
        0,
    );
    Ok(AgentCaptureSyncOutcome::Completed {
        sessions_synced,
        sessions_failed: 0,
        checkpoints_synced,
        checkpoints_failed: 0,
    })
}
