use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::super::context_budget::MemoryAnchorConfidence;

const EPISODE_NAMESPACE: &str = "default";
const MAX_EPISODE_ROOT_ID_BYTES: usize = 120;

// This UUID is part of the persisted identity contract. Changing it would give
// an existing Task Episode a different note ID.
const EPISODE_NOTE_NAMESPACE_V1: Uuid = Uuid::from_bytes([
    0xf2, 0xb4, 0xd3, 0xa0, 0x1c, 0x9e, 0x4f, 0x75, 0x8d, 0x20, 0x2a, 0x6b, 0x7c, 0x8d, 0x9e, 0x01,
]);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EpisodeRootKind {
    Task,
    Intent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceSourcePlane {
    AgentRuntime,
    Session,
    Git,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceKind {
    Intent,
    Task,
    Run,
    Evidence,
    Decision,
    PatchSet,
    Session,
    ToolCall,
    Code,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceVisibility {
    Private,
    RepoLocal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCallPart {
    Invocation,
    Output,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EpistemicStatus {
    Observation,
    Inference,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EpisodeClaimV1 {
    pub(crate) epistemic_status: EpistemicStatus,
    pub(crate) claim: String,
    pub(crate) confidence: Option<MemoryAnchorConfidence>,
    pub(crate) evidence_refs: Vec<EvidenceRefV1>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompletionStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CodeChangeStatus {
    Changed,
    Unchanged,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EpisodeCodeContextV1 {
    pub(crate) base_oid: Option<String>,
    pub(crate) result_oid: Option<String>,
    pub(crate) branch_ref: Option<String>,
    pub(crate) paths: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EpisodeOmissionsV1 {
    pub(crate) related_run_ids: u32,
    pub(crate) observations: u32,
    pub(crate) inferences: u32,
    pub(crate) decisions: u32,
    pub(crate) failed_attempts: u32,
    pub(crate) unresolved: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EpisodePayloadV1 {
    pub(crate) schema_version: u32,
    pub(crate) root_kind: EpisodeRootKind,
    pub(crate) root_id: String,
    pub(crate) related_intent_ids: Vec<String>,
    pub(crate) related_task_ids: Vec<String>,
    pub(crate) related_run_ids: Vec<String>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) goal: EpisodeClaimV1,
    pub(crate) completion_status: CompletionStatus,
    pub(crate) code_change_status: CodeChangeStatus,
    pub(crate) summary: EpisodeClaimV1,
    pub(crate) observations: Vec<EpisodeClaimV1>,
    pub(crate) inferences: Vec<EpisodeClaimV1>,
    pub(crate) decisions: Vec<EpisodeClaimV1>,
    pub(crate) failed_attempts: Vec<EpisodeClaimV1>,
    pub(crate) unresolved: Vec<EpisodeClaimV1>,
    pub(crate) code: EpisodeCodeContextV1,
    pub(crate) omissions: EpisodeOmissionsV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrustedEpisodeFieldsV1 {
    pub(crate) root: EpisodeRoot,
    pub(crate) related_intent_ids: Vec<String>,
    pub(crate) related_task_ids: Vec<String>,
    pub(crate) related_run_ids: Vec<String>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) goal: EpisodeClaimV1,
    pub(crate) completion_status: CompletionStatus,
    pub(crate) code_change_status: CodeChangeStatus,
    pub(crate) code: EpisodeCodeContextV1,
    pub(crate) omissions: EpisodeOmissionsV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryKind {
    Procedural,
    Semantic,
    Episodic,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum MemoryScopeV1 {
    Repo,
    Branch(String),
    Worktree(String),
    Actor(String),
    Global,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryVisibility {
    Private,
    RepoLocal,
    TeamCandidate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryLifecycle {
    Replacement,
    Accretive,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryTrust {
    Verified,
    RepoEvidence,
    UserAsserted,
    ExternalUntrusted,
    Inferred,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemorySensitivity {
    Public,
    Internal,
    Confidential,
    SecretLike,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActorKind {
    Human,
    Agent,
    System,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ActorRefV1 {
    pub(crate) kind: ActorKind,
    pub(crate) principal_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompileOriginV1 {
    Explicit,
    PromotedFromAnchor,
    DistilledFromFrame,
    Classifier,
    Consolidation,
    Onboard,
    BranchFork,
    Import,
    Coordinator,
    EpisodeCompiler,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdempotencyScopeV1 {
    Cell,
    Namespace,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CompileRecordV1 {
    pub(crate) schema_version: u32,
    pub(crate) origin: CompileOriginV1,
    pub(crate) producer: String,
    pub(crate) rules_version: u32,
    pub(crate) prompt_version: Option<String>,
    pub(crate) model_id: Option<String>,
    pub(crate) policy_version: String,
    pub(crate) input_hashes: Vec<String>,
    pub(crate) idempotency_key: String,
    pub(crate) idempotency_scope: IdempotencyScopeV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryLinkKind {
    Sibling,
    Supports,
    Prerequisite,
    Contradicts,
    Supersedes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryLinkV1 {
    pub(crate) kind: MemoryLinkKind,
    pub(crate) target_note_id: Uuid,
    pub(crate) target_revision_oid: Option<String>,
    pub(crate) evidence_refs: Vec<EvidenceRefV1>,
    pub(crate) valid_from: Option<DateTime<Utc>>,
    pub(crate) valid_until: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryEntityRole {
    Subject,
    Object,
    Topic,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryEntityMentionV1 {
    pub(crate) schema_version: u32,
    pub(crate) canonical_key: String,
    pub(crate) display_name: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) role: MemoryEntityRole,
    pub(crate) resolution_confidence: MemoryAnchorConfidence,
    pub(crate) evidence_refs: Vec<EvidenceRefV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryNoteV1 {
    pub(crate) schema_version: u32,
    pub(crate) note_id: Uuid,
    pub(crate) content_digest: String,
    pub(crate) namespace: String,
    pub(crate) path: String,
    pub(crate) kind: MemoryKind,
    pub(crate) scope: MemoryScopeV1,
    pub(crate) visibility: MemoryVisibility,
    pub(crate) acl_policy_id: String,
    pub(crate) lifecycle: MemoryLifecycle,
    pub(crate) body: String,
    pub(crate) rationale: Option<String>,
    pub(crate) episode: Option<EpisodePayloadV1>,
    pub(crate) evidence_refs: Vec<EvidenceRefV1>,
    pub(crate) links: Vec<MemoryLinkV1>,
    pub(crate) entities: Vec<MemoryEntityMentionV1>,
    pub(crate) parents: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) confidence: MemoryAnchorConfidence,
    pub(crate) trust: MemoryTrust,
    pub(crate) sensitivity: MemorySensitivity,
    pub(crate) valid_from: Option<DateTime<Utc>>,
    pub(crate) valid_until: Option<DateTime<Utc>>,
    pub(crate) effective_from_commit: Option<String>,
    pub(crate) effective_until_commit: Option<String>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) author: ActorRefV1,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) compile_record: CompileRecordV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryEventAction {
    Created,
    Revised,
    Confirmed,
    Quarantined,
    Superseded,
    Revoked,
    Forgotten,
    TaxonomyExpanded,
    Consolidated,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryEventV1 {
    pub(crate) schema_version: u32,
    pub(crate) event_id: Uuid,
    pub(crate) event_seq: u64,
    pub(crate) note_id: Option<Uuid>,
    pub(crate) revision_oid: Option<String>,
    pub(crate) namespace: Option<String>,
    pub(crate) target_path: Option<String>,
    pub(crate) action: MemoryEventAction,
    pub(crate) reason_code: Option<String>,
    pub(crate) actor: ActorRefV1,
    pub(crate) at: DateTime<Utc>,
    pub(crate) evidence_refs: Vec<EvidenceRefV1>,
    pub(crate) next_note_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum EvidenceLocatorV1 {
    Object,
    EventSeq {
        event_seq: u64,
    },
    JsonPointer {
        pointer: String,
    },
    SessionFragment {
        start_seq: u64,
        end_seq: u64,
    },
    ToolCall {
        invocation_id: String,
        part: ToolCallPart,
    },
    CodeRange {
        commit_oid: String,
        path: String,
        start_line: u32,
        end_line: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EvidenceRefV1 {
    pub(crate) schema_version: u32,
    pub(crate) source_plane: EvidenceSourcePlane,
    pub(crate) kind: EvidenceKind,
    pub(crate) object_id: String,
    pub(crate) source_ref_oid: String,
    pub(crate) locator: EvidenceLocatorV1,
    pub(crate) fragment_digest: String,
    pub(crate) visibility: EvidenceVisibility,
    pub(crate) captured_at: Option<DateTime<Utc>>,
    pub(crate) code_commit: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EpisodeRoot {
    kind: EpisodeRootKind,
    id: String,
    path: String,
    note_id: Uuid,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum MemoryContractError {
    #[error("episode root ID cannot be empty")]
    EmptyEpisodeRootId,
    #[error("episode root ID must not have surrounding whitespace or control characters")]
    NonCanonicalEpisodeRootId,
    #[error("episode root ID exceeds the {max_bytes}-byte limit")]
    EpisodeRootIdTooLong { max_bytes: usize },
    #[error("unsupported {object} schema version {version}")]
    UnsupportedSchemaVersion { object: &'static str, version: u32 },
    #[error("invalid {field}")]
    InvalidField { field: &'static str },
    #[error("invalid {object} JSON")]
    InvalidJson { object: &'static str },
}

impl EpisodeRoot {
    pub(crate) fn task(id: impl Into<String>) -> Result<Self, MemoryContractError> {
        Self::new(EpisodeRootKind::Task, id.into())
    }

    pub(crate) fn intent(id: impl Into<String>) -> Result<Self, MemoryContractError> {
        Self::new(EpisodeRootKind::Intent, id.into())
    }

    fn new(kind: EpisodeRootKind, id: String) -> Result<Self, MemoryContractError> {
        if id.is_empty() {
            return Err(MemoryContractError::EmptyEpisodeRootId);
        }
        if id.len() > MAX_EPISODE_ROOT_ID_BYTES {
            return Err(MemoryContractError::EpisodeRootIdTooLong {
                max_bytes: MAX_EPISODE_ROOT_ID_BYTES,
            });
        }
        if id.trim() != id || id.chars().any(char::is_control) {
            return Err(MemoryContractError::NonCanonicalEpisodeRootId);
        }

        let encoded_id = hex::encode(id.as_bytes());
        let (identity_prefix, path_prefix) = match kind {
            EpisodeRootKind::Task => (b"task\0".as_slice(), "episodic.tasks"),
            EpisodeRootKind::Intent => (b"intent\0".as_slice(), "episodic.intents"),
        };
        let mut identity_name = identity_prefix.to_vec();
        identity_name.extend_from_slice(id.as_bytes());

        Ok(Self {
            kind,
            path: format!("{path_prefix}.r-{encoded_id}"),
            note_id: Uuid::new_v5(&EPISODE_NOTE_NAMESPACE_V1, &identity_name),
            id,
        })
    }

    pub(crate) fn kind(&self) -> EpisodeRootKind {
        self.kind
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn namespace(&self) -> &'static str {
        EPISODE_NAMESPACE
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn note_id(&self) -> Uuid {
        self.note_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_root_has_stable_cell_identity() {
        let root = EpisodeRoot::task("task-42").expect("synthetic task id is valid");

        assert_eq!(root.kind(), EpisodeRootKind::Task);
        assert_eq!(root.id(), "task-42");
        assert_eq!(root.namespace(), "default");
        assert_eq!(root.path(), "episodic.tasks.r-7461736b2d3432");
        assert_eq!(
            root.note_id().to_string(),
            "98809d1c-f0cd-5e98-84b8-c1dddf5aeb19",
        );
        assert_eq!(
            EpisodeRoot::task(""),
            Err(MemoryContractError::EmptyEpisodeRootId),
        );
    }

    #[test]
    fn intent_root_has_a_distinct_stable_cell_identity() {
        let root = EpisodeRoot::intent("intent-9").expect("synthetic intent id is valid");

        assert_eq!(root.kind(), EpisodeRootKind::Intent);
        assert_eq!(root.id(), "intent-9");
        assert_eq!(root.namespace(), "default");
        assert_eq!(root.path(), "episodic.intents.r-696e74656e742d39");
        assert_eq!(
            root.note_id().to_string(),
            "760369f7-ba78-541a-9aae-4e899154530b",
        );
    }

    #[test]
    fn root_ids_reject_ambiguous_or_unbounded_values() {
        assert_eq!(
            EpisodeRoot::task(" task-42"),
            Err(MemoryContractError::NonCanonicalEpisodeRootId),
        );
        assert_eq!(
            EpisodeRoot::task("task-42\n"),
            Err(MemoryContractError::NonCanonicalEpisodeRootId),
        );
        assert_eq!(
            EpisodeRoot::task("x".repeat(121)),
            Err(MemoryContractError::EpisodeRootIdTooLong { max_bytes: 120 }),
        );
    }

    #[test]
    fn contract_errors_have_stable_redacted_messages() {
        assert_eq!(
            MemoryContractError::UnsupportedSchemaVersion {
                object: "MemoryNote",
                version: 2,
            }
            .to_string(),
            "unsupported MemoryNote schema version 2",
        );
        assert_eq!(
            MemoryContractError::InvalidJson {
                object: "EpisodePayload",
            }
            .to_string(),
            "invalid EpisodePayload JSON",
        );
        assert_eq!(
            MemoryContractError::InvalidField {
                field: "EpisodePayload.summary",
            }
            .to_string(),
            "invalid EpisodePayload.summary",
        );
    }
}
