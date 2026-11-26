//! Read-only Codex summary and typed stored-source presence projection.
//! The original source, transcript bodies, and native artifacts are never read.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use sea_orm::DatabaseTransaction;
use serde::Serialize;

use super::catalog::CaptureCatalogStore;
use crate::internal::ai::{
    checkpoint_reader::{
        CheckpointMetadataRead, CheckpointReadBudget, latest_session_metadata_in_snapshot,
    },
    observed_agents::{
        parse_canon_value,
        session_summary::{SessionSummaryFacts, SummaryTokenUsage},
    },
};

/// Why a summary could not be completed, kept separate from the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SummaryErrorKind {
    Budget,
    Transient,
    Corrupt,
    /// A catalog-level failure (the source itself is unusable).
    Catalog,
}

enum SummaryMetadataSource {
    Snapshot(CheckpointMetadataRead),
    Store {
        checkpoint_id: String,
        storage: std::path::PathBuf,
        tree_oid: git_internal::hash::ObjectHash,
        metadata_oid: git_internal::hash::ObjectHash,
    },
}

impl SummaryMetadataSource {
    fn checkpoint_id(&self) -> &str {
        match self {
            Self::Snapshot(checkpoint) => checkpoint.checkpoint_id(),
            Self::Store { checkpoint_id, .. } => checkpoint_id.as_str(),
        }
    }

    fn read(
        self,
        budget: &CheckpointReadBudget,
    ) -> Result<Vec<u8>, crate::internal::ai::checkpoint_reader::CheckpointReaderError> {
        match self {
            Self::Snapshot(checkpoint) => checkpoint.read(budget),
            Self::Store {
                checkpoint_id,
                storage,
                tree_oid,
                metadata_oid,
            } => read_store_metadata_blob(&storage, &checkpoint_id, tree_oid, metadata_oid, budget),
        }
    }
}

fn read_store_metadata_blob(
    storage: &Path,
    checkpoint_id: &str,
    tree_oid: git_internal::hash::ObjectHash,
    metadata_oid: git_internal::hash::ObjectHash,
    budget: &CheckpointReadBudget,
) -> Result<Vec<u8>, crate::internal::ai::checkpoint_reader::CheckpointReaderError> {
    use git_internal::internal::object::{
        ObjectTrait,
        tree::{Tree, TreeItemMode},
    };

    use crate::internal::ai::checkpoint_reader::CheckpointReaderError;

    budget.check()?;
    if checkpoint_id.len() < 3 || !checkpoint_id.is_ascii() {
        return Err(CheckpointReaderError::RoleProof);
    }
    let client =
        crate::utils::client_storage::ClientStorage::init_local_existing(storage.join("objects"));
    let read_tree = |oid: git_internal::hash::ObjectHash| -> Result<Tree, CheckpointReaderError> {
        let raw = client
            .get(&oid)
            .map_err(|_| CheckpointReaderError::ObjectIntegrity)?;
        Tree::from_bytes(&raw, oid).map_err(|_| CheckpointReaderError::ObjectIntegrity)
    };
    let descend = |tree: &Tree, name: &str| -> Result<Tree, CheckpointReaderError> {
        let item = tree
            .tree_items
            .iter()
            .find(|entry| entry.name == name)
            .ok_or(CheckpointReaderError::UnknownLayout)?;
        if item.mode != TreeItemMode::Tree {
            return Err(CheckpointReaderError::UnknownLayout);
        }
        read_tree(item.id)
    };
    let root = read_tree(tree_oid)?;
    let wrapped = descend(&root, "checkpoint")?;
    let prefix = descend(&wrapped, &checkpoint_id[..2])?;
    let inner = descend(&prefix, &checkpoint_id[2..])?;
    let metadata_entry = inner
        .tree_items
        .iter()
        .find(|entry| entry.name == "metadata.json")
        .ok_or(CheckpointReaderError::RoleProof)?;
    if metadata_entry.mode != TreeItemMode::Blob || metadata_entry.id != metadata_oid {
        return Err(CheckpointReaderError::RoleProof);
    }
    budget.check()?;
    let bytes = client
        .get(&metadata_oid)
        .map_err(|_| CheckpointReaderError::ObjectIntegrity)?;
    if bytes.len() > 1024 * 1024 {
        return Err(CheckpointReaderError::Budget);
    }
    budget.check()?;
    Ok(bytes)
}

pub(crate) struct SessionSummaryRead {
    checkpoint: Option<SummaryMetadataSource>,
    pending: bool,
    budget: CheckpointReadBudget,
    session_id: String,
    agent_kind: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionSummaryView {
    pub(crate) status: &'static str,
    pub(crate) source_status: &'static str,
    pub(crate) as_of_checkpoint: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) prompt_count: Option<u64>,
    pub(crate) token_usage: Option<SummaryTokenUsage>,
    pub(crate) modified_file_count: Option<u64>,
}

impl SessionSummaryView {
    pub(crate) fn unavailable() -> Self {
        Self::empty("unavailable")
    }
    fn empty(status: &'static str) -> Self {
        Self {
            status,
            source_status: "unproven",
            as_of_checkpoint: None,
            model: None,
            title: None,
            prompt_count: None,
            token_usage: None,
            modified_file_count: None,
        }
    }
}

impl SessionSummaryRead {
    /// Capture state, pending status and metadata authority in the same snapshot.
    pub(crate) async fn prepare(
        txn: &DatabaseTransaction,
        storage: &Path,
        session_id: &str,
    ) -> anyhow::Result<Self> {
        let budget =
            CheckpointReadBudget::new(Instant::now() + Duration::from_secs(1), Arc::new(|| false));
        Self::prepare_for(txn, storage, session_id, "codex", budget).await
    }

    /// Prepare with an explicit agent kind and a caller-shared read budget, so
    /// a page of sessions cannot outlive one bounded read window.
    pub(crate) async fn prepare_for(
        txn: &DatabaseTransaction,
        storage: &Path,
        session_id: &str,
        agent_kind: &str,
        budget: CheckpointReadBudget,
    ) -> anyhow::Result<Self> {
        let pending = CaptureCatalogStore::session_summary_is_pending(txn, session_id).await?;
        let checkpoint =
            latest_session_metadata_in_snapshot(txn, storage, session_id, &budget).await?;
        Ok(Self {
            checkpoint: checkpoint.map(SummaryMetadataSource::Snapshot),
            pending,
            budget,
            session_id: session_id.to_owned(),
            agent_kind: agent_kind.to_owned(),
        })
    }

    /// Store-backed counterpart to [`Self::prepare`] using [`crate::storage::CaptureStore`]
    /// and [`crate::storage::ConfigStore`] without opening SQLite.
    #[allow(dead_code)]
    pub(crate) async fn prepare_with_capture(
        capture: &dyn crate::storage::CaptureStore,
        config: &dyn crate::storage::ConfigStore,
        storage: &Path,
        session_id: &str,
    ) -> anyhow::Result<Self> {
        let budget =
            CheckpointReadBudget::new(Instant::now() + Duration::from_secs(1), Arc::new(|| false));
        Self::prepare_for_with_capture(capture, config, storage, session_id, "codex", budget).await
    }

    /// Store-backed counterpart to [`Self::prepare_for`] using
    /// [`crate::storage::CaptureStore`] and [`crate::storage::ConfigStore`].
    #[allow(dead_code)]
    pub(crate) async fn prepare_for_with_capture(
        capture: &dyn crate::storage::CaptureStore,
        config: &dyn crate::storage::ConfigStore,
        storage: &Path,
        session_id: &str,
        agent_kind: &str,
        budget: CheckpointReadBudget,
    ) -> anyhow::Result<Self> {
        budget.check()?;
        let pending =
            CaptureCatalogStore::session_summary_is_pending_with_capture(capture, session_id)
                .await?;
        let mut checkpoints = capture
            .list_checkpoints_by_session(session_id)
            .await
            .map_err(|err| anyhow::anyhow!("list session checkpoints from capture store: {err}"))?;
        checkpoints.retain(|cp| cp.scope == crate::storage::CaptureCheckpointScope::Committed);
        checkpoints.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.checkpoint_id.cmp(&a.checkpoint_id))
        });
        let latest = checkpoints.into_iter().next();
        let checkpoint = match latest {
            Some(cp) => {
                let format_entry =
                    crate::internal::config::ConfigKv::get_with_store(config, "core.objectformat")
                        .await?;
                if format_entry.as_ref().is_some_and(|e| e.encrypted) {
                    anyhow::bail!("encrypted core.objectformat is unsupported for summary read");
                }
                let kind = match format_entry {
                    Some(entry) => {
                        crate::internal::object_format::parse_config_value(&entry.value)?
                    }
                    None => git_internal::hash::HashKind::Sha1,
                };
                let tree_oid =
                    crate::internal::object_format::parse_hex_for_kind(kind, &cp.tree_oid)?;
                let metadata_oid = crate::internal::object_format::parse_hex_for_kind(
                    kind,
                    &cp.metadata_blob_oid,
                )?;
                Some(SummaryMetadataSource::Store {
                    checkpoint_id: cp.checkpoint_id,
                    storage: storage.to_path_buf(),
                    tree_oid,
                    metadata_oid,
                })
            }
            None => None,
        };
        Ok(Self {
            checkpoint,
            pending,
            budget,
            session_id: session_id.to_owned(),
            agent_kind: agent_kind.to_owned(),
        })
    }

    /// The database transaction has ended before the immutable object read.
    pub(crate) async fn finish(self) -> SessionSummaryView {
        self.finish_result()
            .await
            .unwrap_or_else(|_| SessionSummaryView::unavailable())
    }

    /// Like [`Self::finish`], but preserves whether the failure was a bounded
    /// read timeout or an integrity problem so callers can classify it.
    pub(crate) async fn finish_result(self) -> Result<SessionSummaryView, SummaryErrorKind> {
        let pending = self.pending;
        let Some(checkpoint) = self.checkpoint else {
            return Ok(SessionSummaryView::empty(if pending {
                "pending"
            } else {
                "unknown"
            }));
        };
        let id = checkpoint.checkpoint_id().to_owned();
        let session = self.session_id;
        let agent_kind = self.agent_kind;
        let result = tokio::task::spawn_blocking(move || {
            let bytes = checkpoint.read(&self.budget)?;
            parse_canon_value(&bytes)
                .map_err(|_| anyhow::anyhow!("summary metadata is invalid"))?;
            let metadata: serde_json::Value = serde_json::from_slice(&bytes)?;
            for (field, expected) in [
                ("checkpoint_id", id.as_str()),
                ("session_id", session.as_str()),
                ("agent_kind", agent_kind.as_str()),
            ] {
                if metadata.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
                    anyhow::bail!("summary metadata identity is inconsistent");
                }
            }
            let mut view = SessionSummaryView::empty(if pending { "pending" } else { "unknown" });
            view.as_of_checkpoint = Some(id);
            if let Some(facts) = metadata
                .get("extraction")
                .and_then(SessionSummaryFacts::from_extraction)
            {
                view.status = if pending {
                    "pending"
                } else if facts.partial {
                    "partial"
                } else {
                    "available"
                };
                view.model = facts.model;
                view.title = facts.title;
                view.prompt_count = facts.prompt_count;
                view.modified_file_count = facts.modified_file_count;
                view.token_usage = if pending { None } else { facts.token_usage };
            }
            self.budget.check()?;
            Ok::<_, anyhow::Error>(view)
        })
        .await;
        match result {
            Ok(Ok(view)) => Ok(view),
            Ok(Err(error)) => {
                if let Some(reader) = error
                    .downcast_ref::<crate::internal::ai::checkpoint_reader::CheckpointReaderError>(
                ) {
                    Err(match reader {
                        crate::internal::ai::checkpoint_reader::CheckpointReaderError::Budget => {
                            SummaryErrorKind::Budget
                        }
                        crate::internal::ai::checkpoint_reader::CheckpointReaderError::CatalogUnavailable
                        | crate::internal::ai::checkpoint_reader::CheckpointReaderError::CatalogBusy => {
                            SummaryErrorKind::Catalog
                        }
                        _ => SummaryErrorKind::Corrupt,
                    })
                } else if error.to_string().contains("budget") {
                    Err(SummaryErrorKind::Budget)
                } else {
                    Err(SummaryErrorKind::Corrupt)
                }
            }
            Err(_) => Err(SummaryErrorKind::Corrupt),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoredSourcePresence {
    Complete,
    Partial,
    MissingData,
    Unknown,
}

/// Presence is deliberately weaker than readable-thinking or turn completeness.
/// Only role-proven local ordinary content can turn Complete into stored_source.
pub(crate) fn stored_source_presence(
    bytes: &[u8],
    checkpoint_id: &str,
    session_id: &str,
    provider_session_id: &str,
) -> StoredSourcePresence {
    #[derive(serde::Deserialize)]
    struct Metadata<'a> {
        schema_version: u32,
        checkpoint_id: &'a str,
        session_id: &'a str,
        agent_kind: &'a str,
        scope: &'a str,
        provider_session_id: &'a str,
        #[serde(default)]
        transcript_snapshot: Option<super::snapshot::CaptureSnapshotProjection>,
    }
    if bytes.len() > 1024 * 1024 || parse_canon_value(bytes).is_err() {
        return StoredSourcePresence::Unknown;
    }
    let Ok(metadata) = serde_json::from_slice::<Metadata<'_>>(bytes) else {
        return StoredSourcePresence::Unknown;
    };
    if metadata.schema_version != 2
        || metadata.checkpoint_id != checkpoint_id
        || metadata.session_id != session_id
        || metadata.agent_kind != "codex"
        || metadata.scope != "committed"
        || metadata.provider_session_id != provider_session_id
    {
        return StoredSourcePresence::Unknown;
    }
    let Some(snapshot) = metadata.transcript_snapshot else {
        return StoredSourcePresence::Unknown;
    };
    if !snapshot.source_commitment_is_durable_or_absent() {
        return StoredSourcePresence::Unknown;
    }
    match snapshot.completeness {
        super::snapshot::CaptureSnapshotCompleteness::Partial => {
            if snapshot.partial_reason.is_some() {
                StoredSourcePresence::Partial
            } else {
                StoredSourcePresence::Unknown
            }
        }
        super::snapshot::CaptureSnapshotCompleteness::Complete => {
            if snapshot.partial_reason.is_some() {
                return StoredSourcePresence::Unknown;
            }
            if snapshot.source.is_some()
                && snapshot.has_durable_source_commitment()
                && snapshot.transcript_redacted_bytes > 0
            {
                StoredSourcePresence::Complete
            } else {
                StoredSourcePresence::MissingData
            }
        }
    }
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod local_tests {
    use super::*;
    use crate::internal::ai::observed_agents::Redactor;

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_reconcile_typed_source_requires_current_identity_and_source_commitment() {
        let mut metadata = serde_json::json!({
            "schema_version":2, "checkpoint_id":"checkpoint", "session_id":"codex__session",
            "agent_kind":"codex", "provider_session_id":"session", "scope":"committed",
            "transcript_snapshot":{
                "completeness":"complete","partial_reason":null,
                "source":{"kind":"provider_file","identity":"not_retained:v1",
                    "digest_sha256":format!("source/hmac-v2/{}","1".repeat(64)), "byte_len":32},
                "transcript_redacted_bytes":32,"redaction_match_count":0,
                "redaction_bytes_scanned":32,"redaction_bytes_redacted":0
            }
        });
        let parse = |value: &serde_json::Value| {
            stored_source_presence(
                &serde_json::to_vec(value).unwrap(),
                "checkpoint",
                "codex__session",
                "session",
            )
        };
        assert_eq!(parse(&metadata), StoredSourcePresence::Complete);
        for key in [
            "checkpoint_id",
            "session_id",
            "agent_kind",
            "provider_session_id",
            "scope",
        ] {
            let mut bad = metadata.clone();
            bad[key] = "foreign".into();
            assert_eq!(parse(&bad), StoredSourcePresence::Unknown, "{key}");
        }
        let mut legacy = metadata.clone();
        legacy["schema_version"] = 1.into();
        assert_eq!(parse(&legacy), StoredSourcePresence::Unknown);
        legacy
            .as_object_mut()
            .unwrap()
            .remove("transcript_snapshot");
        assert_eq!(parse(&legacy), StoredSourcePresence::Unknown);
        let mut empty = metadata.clone();
        empty["transcript_snapshot"]["source"] = serde_json::Value::Null;
        assert_eq!(parse(&empty), StoredSourcePresence::MissingData);
        empty = metadata.clone();
        empty["transcript_snapshot"]["transcript_redacted_bytes"] = 0.into();
        assert_eq!(parse(&empty), StoredSourcePresence::MissingData);
        metadata["transcript_snapshot"]["source"]["digest_sha256"] = "sha256:transient".into();
        assert_eq!(parse(&metadata), StoredSourcePresence::Unknown);
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_reconcile_partial_and_duplicate_metadata_never_become_complete() {
        let bytes = br#"{"schema_version":2,"checkpoint_id":"c","session_id":"s","agent_kind":"codex","provider_session_id":"p","scope":"committed","transcript_snapshot":{"completeness":"partial","partial_reason":"source_absent","source":null,"transcript_redacted_bytes":0,"redaction_match_count":0,"redaction_bytes_scanned":0,"redaction_bytes_redacted":0}}"#;
        assert_eq!(
            stored_source_presence(bytes, "c", "s", "p"),
            StoredSourcePresence::Partial
        );
        let duplicate = String::from_utf8(bytes.to_vec()).unwrap().replacen(
            "\"schema_version\":2",
            "\"schema_version\":1,\"schema_version\":2",
            1,
        );
        assert_eq!(
            stored_source_presence(duplicate.as_bytes(), "c", "s", "p"),
            StoredSourcePresence::Unknown
        );
        let bad = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("\"source_absent\"", "null");
        assert_eq!(
            stored_source_presence(bad.as_bytes(), "c", "s", "p"),
            StoredSourcePresence::Unknown
        );
        assert_eq!(
            stored_source_presence(&vec![b' '; 1024 * 1024 + 1], "c", "s", "p"),
            StoredSourcePresence::Unknown
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_session_summary_decodes_actual_live_extraction_builder() {
        let raw = include_bytes!(
            "../../../../tests/fixtures/agent_transcripts/codex-capabilities-01621.jsonl"
        );
        let source = Redactor::new_default().redact(raw).0;
        let extraction = super::super::live_checkpoint::build_extraction_metadata(
            "codex",
            Some(&source),
            &[],
            &[],
        );
        let facts = SessionSummaryFacts::from_extraction(&extraction).unwrap();
        assert_eq!(facts.prompt_count, Some(2));
        assert_eq!(facts.modified_file_count, Some(2));
        let usage = facts.token_usage.unwrap();
        assert_eq!(usage.input_tokens, 17);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.total_tokens, Some(24));
        assert_eq!(usage.cached_tokens, Some(4));
        assert_eq!(usage.reasoning_tokens, Some(1));
    }
}
