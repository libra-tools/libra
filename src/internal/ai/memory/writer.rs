use std::{collections::HashSet, path::PathBuf, sync::Arc};

use chrono::Utc;
use git_internal::hash::ObjectHash;
use uuid::Uuid;

use super::{
    admission::AdmittedEpisodeProposal,
    canonical::memory_note_content_digest_v1,
    domain::{MemoryEventAction, MemoryEventV1, MemoryNoteV1},
    error::{MemoryWriterError, MemoryWriterErrorKind},
    job_state::CompileJobLease,
    policy::{
        AuthenticatedMemoryContext, DeterministicMemoryProposal, TrustedMemoryTarget,
        validate_writer_policy,
    },
    source::{EpisodeSourceErrorKind, EpisodeSourceResolver},
    store::{
        ProjectedCell, ProjectionMutation, find_cell, read_memory_ref_head,
        validate_projection_watermark,
    },
    tree::{
        MemoryCommitInput, MemoryEventInput, load_note_bytes, load_snapshot, write_revision_commit,
    },
    validation::{parse_memory_event_v1, parse_memory_note_v1},
};
use crate::{
    internal::{
        ai::{
            keyed_digest::RepositoryKeyedDigest,
            linear_ref::{
                LinearRefTransactionOutcome, OwnedRefSpec, OwnedRefTransportPolicy,
                linear_ref_transaction,
            },
        },
        db,
        workspace::RepoIdentity,
    },
    utils::{object::git_object_hash, util::DATABASE},
};

const WRITER_HEAD_CONFLICT_MAX_RETRIES: usize = 3;
const MAX_REVISION_WALK: usize = 4096;
const MEMORY_EVENT_NAMESPACE_V1: Uuid = Uuid::from_bytes([
    0x80, 0x3a, 0x58, 0x77, 0x40, 0x35, 0x4a, 0xf0, 0xa1, 0xd8, 0xf0, 0x61, 0x52, 0x4d, 0x0b, 0x52,
]);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommittedMemoryEnvelope {
    note_id: Uuid,
    revision_oid: ObjectHash,
    commit_oid: ObjectHash,
    event_seq: u64,
    appended: bool,
}

impl CommittedMemoryEnvelope {
    pub(crate) const fn note_id(&self) -> Uuid {
        self.note_id
    }

    pub(crate) const fn revision_oid(&self) -> ObjectHash {
        self.revision_oid
    }

    pub(crate) const fn commit_oid(&self) -> ObjectHash {
        self.commit_oid
    }

    pub(crate) const fn event_seq(&self) -> u64 {
        self.event_seq
    }

    pub(crate) const fn appended(&self) -> bool {
        self.appended
    }
}

pub(crate) struct MemoryWriter {
    storage_path: PathBuf,
    database: Arc<sea_orm::DatabaseConnection>,
    digest_provider: Arc<RepositoryKeyedDigest>,
    #[cfg(test)]
    test_before_first_cas: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl MemoryWriter {
    pub(crate) async fn open(storage_path: PathBuf) -> Result<Self, MemoryWriterError> {
        if OwnedRefSpec::MemoryRepo.transport_policy() != OwnedRefTransportPolicy::LocalOnly {
            return Err(MemoryWriterError::new(
                MemoryWriterErrorKind::PolicyRejected,
                "Memory ref must use local-only object persistence",
            ));
        }
        let storage_path = tokio::fs::canonicalize(storage_path)
            .await
            .map_err(|error| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::StorageFailure,
                    format!("canonicalize repository storage for Memory writer: {error}"),
                )
            })?;
        let database_path = storage_path.join(DATABASE);
        let database = Arc::new(
            db::get_db_conn_instance_for_path(&database_path)
                .await
                .map_err(|error| {
                    MemoryWriterError::new(
                        MemoryWriterErrorKind::StorageFailure,
                        format!("open repository database for Memory writer: {error}"),
                    )
                })?,
        );
        let digest_provider = RepositoryKeyedDigest::load_or_initialize(&database_path)
            .await
            .map_err(|error| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::DigestKeyUnavailable,
                    error.to_string(),
                )
            })?;
        validate_repository_binding(database.as_ref(), &digest_provider).await?;
        Ok(Self {
            storage_path,
            database,
            digest_provider,
            #[cfg(test)]
            test_before_first_cas: std::sync::Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(in crate::internal::ai::memory) async fn for_tests(
        storage_path: PathBuf,
        database: Arc<sea_orm::DatabaseConnection>,
        digest_provider: Arc<RepositoryKeyedDigest>,
    ) -> Result<Self, MemoryWriterError> {
        validate_repository_binding(database.as_ref(), &digest_provider).await?;
        Ok(Self {
            storage_path,
            database,
            digest_provider,
            test_before_first_cas: std::sync::Mutex::new(None),
        })
    }

    #[cfg(test)]
    fn set_test_before_first_cas(&self, barrier: Arc<tokio::sync::Barrier>) {
        let mut slot = self
            .test_before_first_cas
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *slot = Some(barrier);
    }

    pub(crate) async fn commit_admitted(
        &self,
        resolver: &EpisodeSourceResolver<'_>,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        admitted: &AdmittedEpisodeProposal,
        expected_head: Option<ObjectHash>,
        job_lease: Option<&CompileJobLease>,
    ) -> Result<CommittedMemoryEnvelope, MemoryWriterError> {
        resolver
            .revalidate(context, target, admitted.source())
            .await
            .map_err(|error| {
                let kind = writer_error_kind_for_source(error.kind());
                MemoryWriterError::new(kind, "Episode source evidence could not be revalidated")
            })?;
        self.commit_validated(
            context,
            target,
            admitted.proposal(),
            expected_head,
            job_lease,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn commit(
        &self,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        proposal: &DeterministicMemoryProposal,
        expected_head: Option<ObjectHash>,
    ) -> Result<CommittedMemoryEnvelope, MemoryWriterError> {
        self.commit_validated(context, target, proposal, expected_head, None)
            .await
    }

    async fn commit_validated(
        &self,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        proposal: &DeterministicMemoryProposal,
        expected_head: Option<ObjectHash>,
        job_lease: Option<&CompileJobLease>,
    ) -> Result<CommittedMemoryEnvelope, MemoryWriterError> {
        self.digest_provider
            .validate_for_connection(self.database.as_ref())
            .await
            .map_err(|error| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::DigestKeyUnavailable,
                    error.to_string(),
                )
            })?;
        validate_repository_binding(self.database.as_ref(), &self.digest_provider).await?;
        validate_writer_policy(
            context,
            target,
            proposal,
            self.digest_provider.repository_id(),
            self.digest_provider.key_id(),
        )?;

        let mut last_observed_head = expected_head;
        for attempt in 0..=WRITER_HEAD_CONFLICT_MAX_RETRIES {
            let current_head = read_memory_ref_head(self.database.as_ref()).await?;
            // The caller's expected head is a snapshot hint. A mismatch means
            // the proposal must be rebuilt on current authoritative state.
            let _head_changed_since_request = last_observed_head != current_head;

            let policy_version = &proposal.note().compile_record.policy_version;
            let snapshot = load_snapshot(&self.storage_path, current_head, policy_version)?;
            validate_projection_watermark(
                self.database.as_ref(),
                current_head,
                snapshot.manifest.last_event_seq,
            )
            .await?;

            let cell = find_cell(
                self.database.as_ref(),
                target.root().namespace(),
                target.root().path(),
            )
            .await?;
            if cell
                .as_ref()
                .is_some_and(|cell| cell.note_id != target.root().note_id().to_string())
            {
                return Err(MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptProjection,
                    "Memory Cell points at a non-deterministic note ID",
                ));
            }

            if let Some(cell) = &cell
                && let Some(revision_oid) = self.find_idempotent_revision(
                    cell,
                    &proposal.note().compile_record.idempotency_key,
                )?
            {
                let commit_oid = current_head.ok_or_else(|| {
                    MemoryWriterError::new(
                        MemoryWriterErrorKind::CorruptProjection,
                        "Memory projection exists without an authoritative ref",
                    )
                })?;
                return Ok(CommittedMemoryEnvelope {
                    note_id: target.root().note_id(),
                    revision_oid,
                    commit_oid,
                    event_seq: snapshot.manifest.last_event_seq,
                    appended: false,
                });
            }

            let mut note = proposal.note().clone();
            note.parents = cell
                .as_ref()
                .map(|cell| vec![cell.latest_revision_oid.to_string()])
                .unwrap_or_default();
            note.content_digest = memory_note_content_digest_v1(&note)?;
            let note_bytes = serde_json::to_vec(&note).map_err(|_| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::InvalidProposal,
                    "MemoryNote could not be serialized",
                )
            })?;
            let note = parse_memory_note_v1(&note_bytes)?;
            let revision_oid = git_object_hash("blob", &note_bytes);
            let base_event_seq = snapshot.manifest.last_event_seq;
            let transition_seq = base_event_seq.checked_add(1).ok_or_else(|| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptHistory,
                    "Memory event sequence overflowed",
                )
            })?;
            let action = if cell.is_some() {
                MemoryEventAction::Revised
            } else {
                MemoryEventAction::Created
            };
            let transition_id = event_id(&note, revision_oid, action);
            let transition = MemoryEventV1 {
                schema_version: 1,
                event_id: transition_id,
                event_seq: transition_seq,
                note_id: Some(note.note_id),
                revision_oid: Some(revision_oid.to_string()),
                namespace: None,
                target_path: None,
                action,
                reason_code: Some("episode_compiled".to_string()),
                actor: context.actor().clone(),
                at: note.created_at,
                evidence_refs: note.evidence_refs.clone(),
                next_note_id: None,
            };
            let transition_bytes = serde_json::to_vec(&transition).map_err(|_| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::InvalidProposal,
                    "MemoryEvent could not be serialized",
                )
            })?;
            let transition = parse_memory_event_v1(&transition_bytes)?;
            let event_seq = transition_seq.checked_add(1).ok_or_else(|| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptHistory,
                    "Memory event sequence overflowed",
                )
            })?;
            let confirmed = MemoryEventV1 {
                schema_version: 1,
                event_id: event_id(&note, revision_oid, MemoryEventAction::Confirmed),
                event_seq,
                note_id: Some(note.note_id),
                revision_oid: Some(revision_oid.to_string()),
                namespace: None,
                target_path: None,
                action: MemoryEventAction::Confirmed,
                reason_code: Some("automatic_episode_policy".to_string()),
                actor: context.actor().clone(),
                at: note.created_at,
                evidence_refs: note.evidence_refs.clone(),
                next_note_id: None,
            };
            let confirmed_bytes = serde_json::to_vec(&confirmed).map_err(|_| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::InvalidProposal,
                    "Memory confirmation event could not be serialized",
                )
            })?;
            let confirmed = parse_memory_event_v1(&confirmed_bytes)?;
            let transition_id = transition.event_id.to_string();
            let confirmed_id = confirmed.event_id.to_string();
            let event_inputs = [
                MemoryEventInput {
                    event_seq: transition.event_seq,
                    event_id: &transition_id,
                    event_bytes: &transition_bytes,
                },
                MemoryEventInput {
                    event_seq: confirmed.event_seq,
                    event_id: &confirmed_id,
                    event_bytes: &confirmed_bytes,
                },
            ];

            let objects = write_revision_commit(
                &self.storage_path,
                current_head,
                snapshot,
                MemoryCommitInput {
                    note_id: &note.note_id.to_string(),
                    namespace: &note.namespace,
                    note_bytes: &note_bytes,
                    events: &event_inputs,
                },
            )?;
            if objects.revision_oid != revision_oid {
                return Err(MemoryWriterError::new(
                    MemoryWriterErrorKind::StorageFailure,
                    "MemoryNote object identity changed while writing",
                ));
            }

            let mutation = ProjectionMutation {
                note: note.clone(),
                transition: transition.clone(),
                event: confirmed.clone(),
                revision_oid,
                commit_oid: objects.commit_oid,
                rebuilt_at_ms: Utc::now().timestamp_millis(),
                expected_head: current_head,
                expected_event_seq: base_event_seq,
                expected_cell: cell.clone(),
                repository_id: self.digest_provider.repository_id().to_string(),
                digest_provider: Arc::clone(&self.digest_provider),
                job_lease: job_lease.cloned(),
            };
            #[cfg(test)]
            if attempt == 0 {
                let barrier = self
                    .test_before_first_cas
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if let Some(barrier) = barrier {
                    barrier.wait().await;
                }
            }
            match linear_ref_transaction(
                self.database.as_ref(),
                OwnedRefSpec::MemoryRepo,
                current_head,
                objects.commit_oid,
                None,
                Some(&mutation),
            )
            .await
            .map_err(|error| {
                error
                    .downcast_ref::<MemoryWriterError>()
                    .cloned()
                    .unwrap_or_else(|| {
                        MemoryWriterError::new(
                            MemoryWriterErrorKind::StorageFailure,
                            format!("commit Memory ref and projection transaction failed: {error}"),
                        )
                    })
            })? {
                LinearRefTransactionOutcome::Updated => {
                    return Ok(CommittedMemoryEnvelope {
                        note_id: note.note_id,
                        revision_oid,
                        commit_oid: objects.commit_oid,
                        event_seq,
                        appended: true,
                    });
                }
                LinearRefTransactionOutcome::HeadChanged
                    if attempt < WRITER_HEAD_CONFLICT_MAX_RETRIES =>
                {
                    last_observed_head = current_head;
                }
                LinearRefTransactionOutcome::HeadChanged => {
                    return Err(MemoryWriterError::new(
                        MemoryWriterErrorKind::ConflictExhausted,
                        "Memory ref changed repeatedly while committing a revision",
                    ));
                }
            }
        }

        Err(MemoryWriterError::new(
            MemoryWriterErrorKind::ConflictExhausted,
            "Memory writer exhausted its bounded retry budget",
        ))
    }

    fn find_idempotent_revision(
        &self,
        cell: &ProjectedCell,
        idempotency_key: &str,
    ) -> Result<Option<ObjectHash>, MemoryWriterError> {
        let mut next = Some(cell.latest_revision_oid);
        let mut visited = HashSet::new();
        for _ in 0..MAX_REVISION_WALK {
            let Some(revision_oid) = next else {
                return Ok(None);
            };
            if !visited.insert(revision_oid.to_string()) {
                return Err(MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptHistory,
                    "Memory revision ancestry contains a cycle",
                ));
            }
            let bytes = load_note_bytes(&self.storage_path, revision_oid)?;
            let note = parse_memory_note_v1(&bytes).map_err(|error| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptHistory,
                    format!("persisted Memory revision is invalid: {error}"),
                )
            })?;
            if note.note_id.to_string() != cell.note_id {
                return Err(MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptHistory,
                    "Memory revision ancestry crosses note identities",
                ));
            }
            if note.compile_record.idempotency_key == idempotency_key {
                return Ok(Some(revision_oid));
            }
            next = note
                .parents
                .first()
                .map(|parent| parent.parse())
                .transpose()
                .map_err(|_| {
                    MemoryWriterError::new(
                        MemoryWriterErrorKind::CorruptHistory,
                        "Memory revision contains an invalid parent OID",
                    )
                })?;
        }
        Err(MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptHistory,
            "Memory revision ancestry exceeds the writer traversal bound",
        ))
    }
}

const fn writer_error_kind_for_source(kind: EpisodeSourceErrorKind) -> MemoryWriterErrorKind {
    match kind {
        EpisodeSourceErrorKind::DigestUnavailable => MemoryWriterErrorKind::DigestKeyUnavailable,
        EpisodeSourceErrorKind::LimitExceeded => MemoryWriterErrorKind::SourceLimitExceeded,
        EpisodeSourceErrorKind::Unauthorized
        | EpisodeSourceErrorKind::InvalidRequest
        | EpisodeSourceErrorKind::SourceNotReachable
        | EpisodeSourceErrorKind::DependencyPending
        | EpisodeSourceErrorKind::RedactionFailed => MemoryWriterErrorKind::SourceRejected,
        EpisodeSourceErrorKind::SourceCorrupt => MemoryWriterErrorKind::EvidenceMismatch,
    }
}

async fn validate_repository_binding(
    database: &sea_orm::DatabaseConnection,
    digest_provider: &RepositoryKeyedDigest,
) -> Result<(), MemoryWriterError> {
    let repository = RepoIdentity::resolve(database).await.map_err(|error| {
        MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptProjection,
            format!("repository identity is unavailable to Memory writer: {error}"),
        )
    })?;
    if repository.as_str() != digest_provider.repository_id() {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptProjection,
            "repository database and digest provider identities do not match",
        ));
    }
    Ok(())
}

fn event_id(note: &MemoryNoteV1, revision_oid: ObjectHash, action: MemoryEventAction) -> Uuid {
    let action = match action {
        MemoryEventAction::Created => "created",
        MemoryEventAction::Revised => "revised",
        MemoryEventAction::Confirmed => "confirmed",
        _ => "unsupported",
    };
    let identity = format!(
        "{}\0{}\0{}\0{}",
        note.note_id, note.compile_record.idempotency_key, revision_oid, action
    );
    Uuid::new_v5(&MEMORY_EVENT_NAMESPACE_V1, identity.as_bytes())
}

#[cfg(test)]
pub(in crate::internal::ai::memory) mod tests {
    use std::{fs, sync::Arc};

    use chrono::{TimeZone, Utc};
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
    use serial_test::serial;

    use super::*;
    use crate::{
        internal::{
            ai::{
                context_budget::MemoryAnchorConfidence,
                keyed_digest::RepositoryKeyedDigest,
                memory::{
                    domain::{
                        ActorKind, ActorRefV1, CodeChangeStatus, CompileOriginV1, CompileRecordV1,
                        CompletionStatus, EpisodeClaimV1, EpisodeCodeContextV1, EpisodeOmissionsV1,
                        EpisodePayloadV1, EpisodeRoot, EpisodeRootKind, EpistemicStatus,
                        EvidenceKind, EvidenceLocatorV1, EvidenceRefV1, EvidenceSourcePlane,
                        EvidenceVisibility, IdempotencyScopeV1, MemoryKind, MemoryLifecycle,
                        MemoryNoteV1, MemoryScopeV1, MemorySensitivity, MemoryTrust,
                        MemoryVisibility,
                    },
                    job_state::{CompileJobKey, CompileJobLease},
                    policy::{
                        AuthenticatedMemoryContext, DeterministicMemoryProposal,
                        TrustedMemoryTarget,
                    },
                },
            },
            db::migration::run_builtin_migrations,
        },
        utils::{client_storage::ClientStorage, test::ChangeDirGuard},
    };

    #[test]
    fn source_failures_map_to_stable_writer_categories() {
        for (source, expected) in [
            (
                EpisodeSourceErrorKind::DigestUnavailable,
                MemoryWriterErrorKind::DigestKeyUnavailable,
            ),
            (
                EpisodeSourceErrorKind::LimitExceeded,
                MemoryWriterErrorKind::SourceLimitExceeded,
            ),
            (
                EpisodeSourceErrorKind::SourceNotReachable,
                MemoryWriterErrorKind::SourceRejected,
            ),
            (
                EpisodeSourceErrorKind::SourceCorrupt,
                MemoryWriterErrorKind::EvidenceMismatch,
            ),
        ] {
            assert_eq!(writer_error_kind_for_source(source), expected);
        }
    }

    const REPOSITORY_ID: &str = "memory-writer-test-repository";
    const TEST_CIPHERTEXT: &str = "memory-writer-test-ciphertext";
    const SOURCE_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    pub(crate) struct Fixture {
        pub(crate) _temp: tempfile::TempDir,
        pub(crate) database: Arc<DatabaseConnection>,
        pub(crate) writer: Arc<MemoryWriter>,
        pub(crate) digest: Arc<RepositoryKeyedDigest>,
        pub(crate) context: AuthenticatedMemoryContext,
        pub(crate) target: TrustedMemoryTarget,
        pub(crate) key_id: Uuid,
    }

    pub(crate) async fn fixture() -> Fixture {
        let database = Database::connect("sqlite::memory:")
            .await
            .expect("connect test database");
        let temp = tempfile::tempdir().expect("create object store");
        build_fixture(database, temp).await
    }

    pub(crate) async fn file_backed_fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("create file-backed object store");
        let database_path = temp.path().join("libra.db");
        let mut options = ConnectOptions::new(format!(
            "sqlite://{}?mode=rwc",
            database_path.to_string_lossy()
        ));
        options.max_connections(4);
        let database = Database::connect(options)
            .await
            .expect("connect file-backed test database");
        database
            .execute_unprepared("PRAGMA journal_mode = WAL")
            .await
            .expect("enable WAL for snapshot concurrency test");
        build_fixture(database, temp).await
    }

    async fn build_fixture(database: DatabaseConnection, temp: tempfile::TempDir) -> Fixture {
        database
            .execute_unprepared(include_str!("../../../../sql/sqlite_20260309_init.sql"))
            .await
            .expect("apply bootstrap schema");
        run_builtin_migrations(&database)
            .await
            .expect("apply built-in migrations");
        database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "INSERT INTO config_kv(key, value, encrypted) VALUES
                    ('libra.repoid', ?, 0), (?, ?, 1)",
                [
                    REPOSITORY_ID.into(),
                    "memory.keyed_digest.v1".into(),
                    TEST_CIPHERTEXT.into(),
                ],
            ))
            .await
            .expect("seed digest config");

        let key_id =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("fixed UUID is valid");
        let provider = Arc::new(RepositoryKeyedDigest::for_receipt_tests(
            REPOSITORY_ID,
            key_id,
            [7; 32],
            TEST_CIPHERTEXT,
        ));
        fs::create_dir_all(temp.path().join("objects")).expect("create objects directory");
        let writer = Arc::new(
            MemoryWriter::for_tests(
                temp.path().to_path_buf(),
                Arc::new(database.clone()),
                Arc::clone(&provider),
            )
            .await
            .expect("construct Memory writer"),
        );
        let actor = ActorRefV1 {
            kind: ActorKind::Agent,
            principal_id: "agent:episode-compiler".to_string(),
        };
        let context = AuthenticatedMemoryContext::new(REPOSITORY_ID, actor)
            .expect("construct authenticated context");
        let target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("task-42").expect("construct trusted root"),
        );
        Fixture {
            _temp: temp,
            database: Arc::new(database),
            writer,
            digest: provider,
            context,
            target,
            key_id,
        }
    }

    pub(in crate::internal::ai::memory) fn proposal(
        target: &TrustedMemoryTarget,
        key_id: Uuid,
        generation: u8,
    ) -> DeterministicMemoryProposal {
        let evidence = EvidenceRefV1 {
            schema_version: 1,
            source_plane: EvidenceSourcePlane::Git,
            kind: EvidenceKind::Code,
            object_id: "src/lib.rs".to_string(),
            source_ref_oid: SOURCE_OID.to_string(),
            locator: EvidenceLocatorV1::CodeRange {
                commit_oid: SOURCE_OID.to_string(),
                path: "src/lib.rs".to_string(),
                start_line: 1,
                end_line: 4,
            },
            fragment_digest: format!("sha256:{}", "b".repeat(64)),
            visibility: EvidenceVisibility::RepoLocal,
            captured_at: Utc.with_ymd_and_hms(2026, 8, 24, 8, 0, 0).single(),
            code_commit: Some(SOURCE_OID.to_string()),
        };
        let observation = EpisodeClaimV1 {
            epistemic_status: EpistemicStatus::Observation,
            claim: "the focused test failed before the retry fix".to_string(),
            confidence: None,
            evidence_refs: vec![evidence.clone()],
        };
        let inference = EpisodeClaimV1 {
            epistemic_status: EpistemicStatus::Inference,
            claim: format!("generation {generation} attributes the failure to retry timing"),
            confidence: Some(MemoryAnchorConfidence::High),
            evidence_refs: vec![evidence.clone()],
        };
        let episode = EpisodePayloadV1 {
            schema_version: 1,
            root_kind: EpisodeRootKind::Task,
            root_id: target.root().id().to_string(),
            related_intent_ids: Vec::new(),
            related_task_ids: vec![target.root().id().to_string()],
            related_run_ids: vec![format!("run-{generation}")],
            started_at: Utc.with_ymd_and_hms(2026, 8, 24, 8, 0, 0).single(),
            ended_at: Utc
                .with_ymd_and_hms(2026, 8, 24, 9, u32::from(generation), 0)
                .single(),
            goal: observation.clone(),
            completion_status: CompletionStatus::Completed,
            code_change_status: CodeChangeStatus::Changed,
            summary: inference.clone(),
            observations: vec![observation],
            inferences: vec![inference],
            decisions: Vec::new(),
            failed_attempts: Vec::new(),
            unresolved: Vec::new(),
            code: EpisodeCodeContextV1 {
                base_oid: Some(SOURCE_OID.to_string()),
                result_oid: Some(SOURCE_OID.to_string()),
                branch_ref: Some("refs/heads/main".to_string()),
                paths: vec!["src/lib.rs".to_string()],
            },
            omissions: EpisodeOmissionsV1::default(),
        };
        let keyed = |fill: char| format!("hmac-sha256:{key_id}:{}", fill.to_string().repeat(64));
        let note = MemoryNoteV1 {
            schema_version: 1,
            note_id: target.root().note_id(),
            content_digest: format!("sha256:{}", "0".repeat(64)),
            namespace: target.root().namespace().to_string(),
            path: target.root().path().to_string(),
            kind: MemoryKind::Episodic,
            scope: MemoryScopeV1::Repo,
            visibility: MemoryVisibility::RepoLocal,
            acl_policy_id: "repo-default-v1".to_string(),
            lifecycle: MemoryLifecycle::Accretive,
            body: format!("Task episode generation {generation}"),
            rationale: None,
            episode: Some(episode),
            evidence_refs: vec![evidence],
            links: Vec::new(),
            entities: Vec::new(),
            parents: Vec::new(),
            tags: vec!["episode".to_string()],
            confidence: MemoryAnchorConfidence::High,
            trust: MemoryTrust::RepoEvidence,
            sensitivity: MemorySensitivity::Internal,
            valid_from: None,
            valid_until: None,
            effective_from_commit: Some(SOURCE_OID.to_string()),
            effective_until_commit: None,
            expires_at: None,
            author: ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "agent:episode-compiler".to_string(),
            },
            created_at: Utc
                .with_ymd_and_hms(2026, 8, 24, 9, u32::from(generation), 0)
                .single()
                .expect("valid fixture timestamp"),
            compile_record: CompileRecordV1 {
                schema_version: 1,
                origin: CompileOriginV1::EpisodeCompiler,
                producer: "libra-memory/1".to_string(),
                rules_version: 1,
                prompt_version: Some("episode-v1".to_string()),
                model_id: Some("deterministic-test-model".to_string()),
                policy_version: "repo-policy-v1".to_string(),
                input_hashes: vec![keyed(char::from(b'c' + generation))],
                idempotency_key: keyed(char::from(b'd' + generation)),
                idempotency_scope: IdempotencyScopeV1::Cell,
            },
        };
        DeterministicMemoryProposal::new(note)
    }

    async fn count(database: &DatabaseConnection, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) AS count FROM {table}");
        database
            .query_one_raw(Statement::from_string(database.get_database_backend(), sql))
            .await
            .expect("query projection count")
            .expect("count row exists")
            .try_get("", "count")
            .expect("decode projection count")
    }

    async fn memory_ref_count(database: &DatabaseConnection) -> i64 {
        database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM reference
                 WHERE kind = 'Branch' AND remote IS NULL
                   AND name = 'libra/memory/repo'"
                    .to_string(),
            ))
            .await
            .expect("query Memory ref count")
            .expect("Memory ref count row exists")
            .try_get("", "count")
            .expect("decode Memory ref count")
    }

    #[tokio::test]
    async fn writer_round_trip() {
        let fixture = fixture().await;
        let committed = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 1),
                None,
            )
            .await
            .expect("commit first Memory revision");
        assert!(committed.appended());
        assert_eq!(committed.event_seq(), 2);
        assert_eq!(committed.note_id(), fixture.target.root().note_id());
        assert_eq!(count(&fixture.database, "memory_note_index").await, 1);
        assert_eq!(count(&fixture.database, "memory_revision_index").await, 1);
        assert_eq!(count(&fixture.database, "memory_head").await, 1);
        assert_eq!(count(&fixture.database, "memory_projection_state").await, 1);
        let head = fixture
            .database
            .query_one_raw(Statement::from_string(
                fixture.database.get_database_backend(),
                "SELECT latest_review_state, live_revision_oid FROM memory_head".to_string(),
            ))
            .await
            .expect("query committed Memory head")
            .expect("Memory head exists");
        let review_state: String = head
            .try_get("", "latest_review_state")
            .expect("decode review state");
        let live_revision: String = head
            .try_get("", "live_revision_oid")
            .expect("decode live revision");
        assert_eq!(review_state, "confirmed");
        assert_eq!(live_revision, committed.revision_oid().to_string());
    }

    #[tokio::test]
    async fn writer_fences_reclaimed_generation_before_ref_cas() {
        let fixture = fixture().await;
        let source_oid: ObjectHash = SOURCE_OID.parse().expect("fixed source OID");
        let fingerprint = fixture
            .digest
            .source_input_fingerprint(b"writer-fence-test")
            .expect("derive source fingerprint");
        let now = Utc::now().timestamp_millis();
        fixture
            .database
            .execute_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "INSERT INTO memory_compile_job (
                    scope_key, root_kind, root_id, terminal_source_oid,
                    input_fingerprint_version, input_fingerprint_key_id,
                    input_fingerprint_digest, observed_generation,
                    processed_generation, state, lease_owner, lease_fence,
                    lease_expires_at, created_at, updated_at
                 ) VALUES ('repo', 'task', ?, ?, ?, ?, ?, 1, 0, 'inflight',
                           'runner-new', 2, ?, ?, ?)",
                [
                    fixture.target.root().id().into(),
                    source_oid.to_string().into(),
                    i64::from(fingerprint.version()).into(),
                    fingerprint.key_id().to_string().into(),
                    fingerprint.digest_hex().into(),
                    now.saturating_add(30_000).into(),
                    now.into(),
                    now.into(),
                ],
            ))
            .await
            .expect("seed reclaimed generation lease");
        let stale = CompileJobLease::from_persisted(
            CompileJobKey::new("repo", fixture.target.root().clone()).expect("job key"),
            "runner-old".to_string(),
            1,
            1,
            source_oid,
            fingerprint,
        )
        .expect("construct stale lease snapshot");

        let error = fixture
            .writer
            .commit_validated(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 1),
                None,
                Some(&stale),
            )
            .await
            .expect_err("reclaimed lease must fence the Memory ref CAS");
        assert_eq!(error.kind(), MemoryWriterErrorKind::ProjectionStale);
        assert_eq!(memory_ref_count(&fixture.database).await, 0);
        assert_eq!(count(&fixture.database, "memory_revision_index").await, 0);
    }

    #[tokio::test]
    async fn writer_same_key_idempotent() {
        let fixture = fixture().await;
        let proposal = proposal(&fixture.target, fixture.key_id, 1);
        let first = fixture
            .writer
            .commit(&fixture.context, &fixture.target, &proposal, None)
            .await
            .expect("commit first Memory revision");
        let second = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal,
                Some(first.commit_oid()),
            )
            .await
            .expect("deduplicate Memory revision");
        assert!(!second.appended());
        assert_eq!(second.revision_oid(), first.revision_oid());
        assert_eq!(count(&fixture.database, "memory_revision_index").await, 1);
    }

    #[tokio::test]
    async fn writer_rejects_untrusted_acl_and_producer() {
        let fixture = fixture().await;
        let mut untrusted_acl = proposal(&fixture.target, fixture.key_id, 1);
        untrusted_acl.note_mut().acl_policy_id = "arbitrary-policy".to_string();
        let error = fixture
            .writer
            .commit(&fixture.context, &fixture.target, &untrusted_acl, None)
            .await
            .expect_err("unknown ACL policy is rejected");
        assert_eq!(error.kind(), MemoryWriterErrorKind::PolicyRejected);
        assert_eq!(error.stable_code(), "LBR-MEMORY-003");

        let mut untrusted_producer = proposal(&fixture.target, fixture.key_id, 1);
        untrusted_producer.note_mut().compile_record.producer = "external-agent/1".to_string();
        let error = fixture
            .writer
            .commit(&fixture.context, &fixture.target, &untrusted_producer, None)
            .await
            .expect_err("unknown producer is rejected");
        assert_eq!(error.kind(), MemoryWriterErrorKind::PolicyRejected);
        assert_eq!(error.stable_code(), "LBR-MEMORY-003");

        let mut secret = proposal(&fixture.target, fixture.key_id, 1);
        secret.note_mut().sensitivity = MemorySensitivity::SecretLike;
        let error = fixture
            .writer
            .commit(&fixture.context, &fixture.target, &secret, None)
            .await
            .expect_err("secret-like Memory is rejected before object persistence");
        assert_eq!(error.kind(), MemoryWriterErrorKind::PolicyRejected);
        assert_eq!(memory_ref_count(&fixture.database).await, 0);
    }

    #[tokio::test]
    async fn writer_creates_two_distinct_episode_cells() {
        let fixture = fixture().await;
        let first = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 1),
                None,
            )
            .await
            .expect("commit first Episode cell");
        let second_target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("task-43").expect("construct second trusted root"),
        );
        let second = fixture
            .writer
            .commit(
                &fixture.context,
                &second_target,
                &proposal(&second_target, fixture.key_id, 1),
                Some(first.commit_oid()),
            )
            .await
            .expect("commit second Episode cell");
        assert!(second.appended());
        assert_eq!(second.event_seq(), 4);
        assert_eq!(count(&fixture.database, "memory_note_index").await, 2);
    }

    #[tokio::test]
    async fn writer_fails_closed_on_duplicate_cell_heads() {
        let fixture = fixture().await;
        let committed = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 1),
                None,
            )
            .await
            .expect("commit first Memory revision");
        fixture
            .database
            .execute_unprepared(
                "INSERT INTO memory_note_index
                 SELECT '00000000-0000-4000-8000-000000000002', scope_key, namespace, path,
                        kind, lifecycle, review_state, confidence, trust, sensitivity, visibility,
                        acl_policy_id, origin, 'duplicate-cell-key', idempotency_scope, created_at
                 FROM memory_note_index;
                 INSERT INTO memory_revision_index
                 SELECT 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                        '00000000-0000-4000-8000-000000000002', scope_key, namespace, origin,
                        producer, rules_version, prompt_version, model_id, policy_version,
                        input_fingerprints_json, created_at
                 FROM memory_revision_index;
                 INSERT INTO memory_head
                 SELECT scope_key, namespace, path,
                        '00000000-0000-4000-8000-000000000002',
                        'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                        'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', latest_action,
                        latest_review_state, kind, lifecycle, confidence, trust, sensitivity,
                        visibility, acl_policy_id, valid_from, valid_until, effective_from_commit,
                        effective_until_commit, expires_at, rank_hint, last_event_seq, updated_at
                 FROM memory_head;",
            )
            .await
            .expect("seed duplicate Cell head corruption");
        let error = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 2),
                Some(committed.commit_oid()),
            )
            .await
            .expect_err("duplicate Cell heads fail closed");
        assert_eq!(error.kind(), MemoryWriterErrorKind::CorruptProjection);
        assert_eq!(error.stable_code(), "LBR-MEMORY-004");
    }

    #[tokio::test]
    async fn production_open_round_trip_revalidates_persisted_key() {
        let temp = tempfile::tempdir().expect("create production writer repository");
        let storage = temp.path().join(".libra");
        fs::create_dir_all(storage.join("objects")).expect("create production object store");
        let database_path = storage.join(DATABASE);
        let database = db::create_database(&database_path.to_string_lossy())
            .await
            .expect("create production repository database");
        database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "INSERT INTO config_kv(key, value, encrypted) VALUES
                    ('libra.repoid', ?, 0), ('vault.unsealkey', ?, 0)",
                [REPOSITORY_ID.into(), hex::encode([0x42_u8; 32]).into()],
            ))
            .await
            .expect("seed production repository identity and vault key");
        database.close().await.expect("close setup connection");

        let writer = MemoryWriter::open(storage.clone())
            .await
            .expect("open production Memory writer");
        let context = AuthenticatedMemoryContext::new(
            REPOSITORY_ID,
            ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "agent:episode-compiler".to_string(),
            },
        )
        .expect("construct production context");
        let target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("task-production").expect("construct production target"),
        );
        let committed = writer
            .commit(
                &context,
                &target,
                &proposal(&target, writer.digest_provider.key_id(), 1),
                None,
            )
            .await
            .expect("commit through production writer");
        assert!(committed.appended());

        writer
            .database
            .execute_unprepared(
                "UPDATE config_kv SET value = 'changed-ciphertext'
                 WHERE key = 'memory.keyed_digest.v1'",
            )
            .await
            .expect("corrupt persisted digest binding");
        let error = writer
            .commit(
                &context,
                &target,
                &proposal(&target, writer.digest_provider.key_id(), 2),
                Some(committed.commit_oid()),
            )
            .await
            .expect_err("changed persisted digest key fails closed");
        assert_eq!(error.kind(), MemoryWriterErrorKind::DigestKeyUnavailable);
        assert_eq!(error.stable_code(), "LBR-MEMORY-001");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn memory_authority_is_a_reachable_gc_root() {
        let temp = tempfile::tempdir().expect("create repository");
        let storage_path = temp.path().join(".libra");
        fs::create_dir_all(storage_path.join("objects")).expect("create object store");
        let database_path = storage_path.join(DATABASE);
        let database = db::create_database(&database_path.to_string_lossy())
            .await
            .expect("create repository database");
        database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "INSERT INTO config_kv(key, value, encrypted) VALUES
                    ('libra.repoid', ?, 0), ('vault.unsealkey', ?, 0)",
                [REPOSITORY_ID.into(), hex::encode([0x24_u8; 32]).into()],
            ))
            .await
            .expect("seed repository identity and vault key");
        database.close().await.expect("close setup connection");

        let writer = MemoryWriter::open(storage_path.clone())
            .await
            .expect("open Memory writer");
        let context = AuthenticatedMemoryContext::new(
            REPOSITORY_ID,
            ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "agent:episode-compiler".to_string(),
            },
        )
        .expect("construct context");
        let target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("task-gc-root").expect("construct target"),
        );
        let committed = writer
            .commit(
                &context,
                &target,
                &proposal(&target, writer.digest_provider.key_id(), 1),
                None,
            )
            .await
            .expect("commit Memory authority");

        let _cwd = ChangeDirGuard::new(temp.path());
        let storage = ClientStorage::init_local_existing(storage_path.join("objects"));
        let reachable = crate::command::maintenance::collect_reachable_objects_with_conn(
            &storage,
            writer.database.as_ref(),
        )
        .await
        .expect("collect GC roots");
        assert!(reachable.contains(&committed.commit_oid()));
        assert!(reachable.contains(&committed.revision_oid()));
        assert!(
            reachable.len() >= 6,
            "Memory commit closure must include commit, tree, event and note objects"
        );
    }

    #[tokio::test]
    async fn writer_concurrent_first_create() {
        let fixture = fixture().await;
        fixture
            .writer
            .set_test_before_first_cas(Arc::new(tokio::sync::Barrier::new(2)));
        let proposal = proposal(&fixture.target, fixture.key_id, 1);
        let (first, second) = tokio::join!(
            fixture
                .writer
                .commit(&fixture.context, &fixture.target, &proposal, None),
            fixture
                .writer
                .commit(&fixture.context, &fixture.target, &proposal, None),
        );
        let first = first.expect("first concurrent writer succeeds");
        let second = second.expect("second concurrent writer converges");
        assert_eq!(first.note_id(), second.note_id());
        assert_eq!(first.revision_oid(), second.revision_oid());
        assert_eq!(count(&fixture.database, "memory_revision_index").await, 1);
    }

    #[tokio::test]
    async fn writer_cas_rebuilds_revision() {
        let fixture = fixture().await;
        let first = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 1),
                None,
            )
            .await
            .expect("commit first revision");
        let second = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 2),
                None,
            )
            .await
            .expect("rebuild proposal on current Memory head");
        assert!(second.appended());
        assert_eq!(second.event_seq(), 4);
        assert_ne!(first.revision_oid(), second.revision_oid());
        assert_eq!(count(&fixture.database, "memory_revision_index").await, 2);
        let note_row = fixture
            .database
            .query_one_raw(Statement::from_string(
                fixture.database.get_database_backend(),
                "SELECT idempotency_key, origin FROM memory_note_index".to_string(),
            ))
            .await
            .expect("query note creation identity")
            .expect("note projection exists");
        let first_key: String = note_row
            .try_get("", "idempotency_key")
            .expect("decode creation key");
        let origin: String = note_row.try_get("", "origin").expect("decode origin");
        assert_eq!(
            first_key,
            proposal(&fixture.target, fixture.key_id, 1)
                .note()
                .compile_record
                .idempotency_key
        );
        assert_eq!(origin, "episode_compiler");
    }

    #[tokio::test]
    async fn writer_fault_windows() {
        let object_fault = fixture().await;
        fs::remove_dir_all(object_fault._temp.path().join("objects"))
            .expect("remove object directory");
        fs::write(object_fault._temp.path().join("objects"), b"blocked")
            .expect("block object directory creation");
        let error = object_fault
            .writer
            .commit(
                &object_fault.context,
                &object_fault.target,
                &proposal(&object_fault.target, object_fault.key_id, 1),
                None,
            )
            .await
            .expect_err("object fault rejects commit");
        assert_eq!(error.kind(), MemoryWriterErrorKind::StorageFailure);
        assert_eq!(count(&object_fault.database, "memory_head").await, 0);
        assert_eq!(memory_ref_count(&object_fault.database).await, 0);

        let projection_fault = fixture().await;
        projection_fault
            .database
            .execute_unprepared(
                "CREATE TRIGGER memory_writer_projection_fault
                 BEFORE INSERT ON memory_revision_index
                 BEGIN SELECT RAISE(ABORT, 'injected projection fault'); END;",
            )
            .await
            .expect("inject projection failure");
        let error = projection_fault
            .writer
            .commit(
                &projection_fault.context,
                &projection_fault.target,
                &proposal(&projection_fault.target, projection_fault.key_id, 1),
                None,
            )
            .await
            .expect_err("projection fault rejects commit");
        assert_eq!(error.kind(), MemoryWriterErrorKind::StorageFailure);
        assert_eq!(
            count(&projection_fault.database, "memory_note_index").await,
            0
        );
        assert_eq!(
            count(&projection_fault.database, "memory_revision_index").await,
            0
        );
        assert_eq!(
            count(&projection_fault.database, "memory_projection_state").await,
            0
        );
        assert_eq!(memory_ref_count(&projection_fault.database).await, 0);
    }

    #[tokio::test]
    async fn writer_rejects_unknown_digest_key_with_stable_code() {
        let fixture = fixture().await;
        let mut proposal = proposal(&fixture.target, fixture.key_id, 1);
        proposal.note_mut().compile_record.idempotency_key = format!(
            "hmac-sha256:550e8400-e29b-41d4-a716-446655440001:{}",
            "e".repeat(64)
        );
        let error = fixture
            .writer
            .commit(&fixture.context, &fixture.target, &proposal, None)
            .await
            .expect_err("unknown digest key is rejected");
        assert_eq!(error.kind(), MemoryWriterErrorKind::UnknownDigestKey);
        assert_eq!(error.stable_code(), "LBR-MEMORY-003");
    }

    #[tokio::test]
    async fn writer_repository_boundaries_fail_closed() {
        let missing_key = fixture().await;
        missing_key
            .database
            .execute_unprepared("DELETE FROM config_kv WHERE key = 'memory.keyed_digest.v1'")
            .await
            .expect("remove digest config");
        let error = missing_key
            .writer
            .commit(
                &missing_key.context,
                &missing_key.target,
                &proposal(&missing_key.target, missing_key.key_id, 1),
                None,
            )
            .await
            .expect_err("missing digest config blocks Memory writes");
        assert_eq!(error.kind(), MemoryWriterErrorKind::DigestKeyUnavailable);
        assert_eq!(error.stable_code(), "LBR-MEMORY-001");

        let missing_identity = fixture().await;
        missing_identity
            .database
            .execute_unprepared("DELETE FROM config_kv WHERE key = 'libra.repoid'")
            .await
            .expect("remove repository identity");
        let error = missing_identity
            .writer
            .commit(
                &missing_identity.context,
                &missing_identity.target,
                &proposal(&missing_identity.target, missing_identity.key_id, 1),
                None,
            )
            .await
            .expect_err("missing repository identity blocks Memory writes");
        assert_eq!(error.kind(), MemoryWriterErrorKind::CorruptProjection);
        assert_eq!(error.stable_code(), "LBR-MEMORY-004");
    }

    #[tokio::test]
    async fn writer_corrupt_revision_fails_loud() {
        let fixture = fixture().await;
        let committed = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 2),
                None,
            )
            .await
            .expect("commit first Memory revision");
        let oid = committed.revision_oid().to_string();
        fs::remove_file(
            fixture
                ._temp
                .path()
                .join("objects")
                .join(&oid[..2])
                .join(&oid[2..]),
        )
        .expect("remove authoritative revision object");
        let error = fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 1),
                Some(committed.commit_oid()),
            )
            .await
            .expect_err("missing authoritative revision fails loud");
        assert_eq!(error.kind(), MemoryWriterErrorKind::CorruptHistory);
        assert_eq!(error.stable_code(), "LBR-MEMORY-004");
    }
}
