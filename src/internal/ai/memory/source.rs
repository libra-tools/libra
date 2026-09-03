use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use git_internal::hash::ObjectHash;
use regex::bytes::Regex;
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    domain::{
        CodeChangeStatus, CompletionStatus, EpisodeCodeContextV1, EpisodeRootKind, EvidenceKind,
        EvidenceLocatorV1, EvidenceRefV1, EvidenceSourcePlane, EvidenceVisibility, MemoryNoteV1,
        ToolCallPart,
    },
    limits::EpisodeSourceLimits,
    policy::{AuthenticatedMemoryContext, TrustedMemoryTarget},
    store::{read_memory_ref_head, validate_projection_watermark},
    tree::{load_history_delta_bounded, load_note_bytes, load_snapshot},
    validation::parse_memory_note_v1,
};
use crate::internal::ai::{
    history::{HistoryManager, PinnedHistoryBlob, PinnedHistoryView},
    keyed_digest::RepositoryKeyedDigest,
    observed_agents::Redactor,
};

const SOURCE_SCHEMA_VERSION: u32 = 1;
const SOURCE_POLICY_VERSION: &str = "repo-episode-source-v1";
const REDACTION_POLICY_VERSION: &str = "memory-redaction-v1";

const TASK: &str = "task";
const INTENT: &str = "intent";
const RUN: &str = "run";
const TASK_EVENT: &str = "task_event";
const INTENT_EVENT: &str = "intent_event";
const RUN_EVENT: &str = "run_event";
const EVIDENCE: &str = "evidence";
const DECISION: &str = "decision";
const PATCHSET: &str = "patchset";
const CONTEXT_FRAME: &str = "context_frame";
const INVOCATION: &str = "invocation";
const TASK_EPISODE: &str = "task_episode";
const MAX_INTENT_TASKS: usize = 128;
const MAX_MEMORY_DEPENDENCY_COMMITS: usize = 2_048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeSourceErrorKind {
    Unauthorized,
    InvalidRequest,
    SourceNotReachable,
    SourceCorrupt,
    LimitExceeded,
    RedactionFailed,
    DigestUnavailable,
    DependencyPending,
}

#[derive(Debug, Error)]
#[error("Episode source resolution failed ({kind:?})")]
pub(crate) struct EpisodeSourceError {
    kind: EpisodeSourceErrorKind,
}

impl EpisodeSourceError {
    const fn new(kind: EpisodeSourceErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> EpisodeSourceErrorKind {
        self.kind
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SourceOmissionV1 {
    pub(crate) code: String,
    pub(crate) object_type: String,
    pub(crate) count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EpisodeSourceLimitSnapshotV1 {
    pub(crate) max_objects: usize,
    pub(crate) max_candidate_objects: usize,
    pub(crate) max_tree_bytes: u64,
    pub(crate) max_object_bytes: u64,
    pub(crate) max_total_bytes: usize,
    pub(crate) max_context_fragments: usize,
    pub(crate) max_token_estimate: usize,
    pub(crate) max_ancestry_commits: usize,
}

impl From<EpisodeSourceLimits> for EpisodeSourceLimitSnapshotV1 {
    fn from(limits: EpisodeSourceLimits) -> Self {
        Self {
            max_objects: limits.max_objects,
            max_candidate_objects: limits.max_candidate_objects,
            max_tree_bytes: limits.max_tree_bytes,
            max_object_bytes: limits.max_object_bytes,
            max_total_bytes: limits.max_total_bytes,
            max_context_fragments: limits.max_context_fragments,
            max_token_estimate: limits.max_token_estimate,
            max_ancestry_commits: limits.max_ancestry_commits,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SourceManifestFragmentV1 {
    pub(crate) fragment_id: String,
    pub(crate) object_type: String,
    pub(crate) object_id: String,
    pub(crate) object_oid: String,
    pub(crate) source_ref_oid: String,
    pub(crate) locator: EvidenceLocatorV1,
    pub(crate) fragment_digest: String,
    pub(crate) code_commit: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PinnedTaskEpisodeV1 {
    task_id: String,
    note_id: uuid::Uuid,
    revision_oid: ObjectHash,
    fragment_id: String,
}

impl PinnedTaskEpisodeV1 {
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    pub(crate) const fn note_id(&self) -> uuid::Uuid {
        self.note_id
    }

    pub(crate) const fn revision_oid(&self) -> ObjectHash {
        self.revision_oid
    }

    pub(crate) fn fragment_id(&self) -> &str {
        &self.fragment_id
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EpisodeSourceManifestV1 {
    pub(crate) schema_version: u32,
    pub(crate) policy_version: String,
    pub(crate) redaction_policy_version: String,
    pub(crate) root_kind: EpisodeRootKind,
    pub(crate) root_id: String,
    pub(crate) repository_id: String,
    pub(crate) principal_digest: String,
    pub(crate) source_ref_oid: String,
    pub(crate) dependency_ref_oid: Option<String>,
    pub(crate) limits: EpisodeSourceLimitSnapshotV1,
    pub(crate) object_count: usize,
    pub(crate) redacted_bytes: usize,
    pub(crate) token_estimate: usize,
    pub(crate) fragments: Vec<SourceManifestFragmentV1>,
    pub(crate) omissions: Vec<SourceOmissionV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EpisodeSourceFacts {
    pub(crate) related_intent_ids: Vec<String>,
    pub(crate) related_task_ids: Vec<String>,
    pub(crate) related_run_ids: Vec<String>,
    pub(crate) root_goal: String,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: DateTime<Utc>,
    pub(crate) completion_status: CompletionStatus,
    pub(crate) code_change_status: CodeChangeStatus,
    pub(crate) code: EpisodeCodeContextV1,
}

/// Redacted source fragment. Its content intentionally has no serde or Debug
/// implementation so ordinary manifests and diagnostics cannot print it.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RedactedEpisodeFragment {
    fragment_id: String,
    object_type: String,
    object_id: String,
    object_oid: ObjectHash,
    text: String,
    evidence: EvidenceRefV1,
}

impl RedactedEpisodeFragment {
    pub(crate) fn fragment_id(&self) -> &str {
        &self.fragment_id
    }

    pub(crate) fn object_type(&self) -> &str {
        &self.object_type
    }

    pub(crate) fn object_id(&self) -> &str {
        &self.object_id
    }

    pub(crate) const fn object_oid(&self) -> ObjectHash {
        self.object_oid
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn evidence(&self) -> &EvidenceRefV1 {
        &self.evidence
    }
}

/// Compiler input that can only be created by [`EpisodeSourceResolver`].
pub(crate) struct RedactedEpisodeSource {
    manifest: EpisodeSourceManifestV1,
    facts: EpisodeSourceFacts,
    fragments: Vec<RedactedEpisodeFragment>,
    pinned_task_episodes: Vec<PinnedTaskEpisodeV1>,
}

impl RedactedEpisodeSource {
    pub(crate) fn manifest(&self) -> &EpisodeSourceManifestV1 {
        &self.manifest
    }

    pub(crate) fn facts(&self) -> &EpisodeSourceFacts {
        &self.facts
    }

    pub(crate) fn fragments(&self) -> &[RedactedEpisodeFragment] {
        &self.fragments
    }

    pub(crate) fn pinned_task_episodes(&self) -> &[PinnedTaskEpisodeV1] {
        &self.pinned_task_episodes
    }

    pub(crate) fn evidence(&self, fragment_id: &str) -> Option<&EvidenceRefV1> {
        self.fragments
            .iter()
            .find(|fragment| fragment.fragment_id == fragment_id)
            .map(RedactedEpisodeFragment::evidence)
    }

    #[cfg(test)]
    pub(crate) fn for_compiler_test(
        root_kind: EpisodeRootKind,
        fragments: &[(&str, &str, &str)],
        completion_status: CompletionStatus,
        code_change_status: CodeChangeStatus,
    ) -> Self {
        let limits = EpisodeSourceLimits::repo_v1();
        let source_ref_oid = ObjectHash::new(b"compiler-test-source").to_string();
        let mut manifest_fragments = Vec::new();
        let mut redacted_fragments = Vec::new();
        for (fragment_id, object_type, text) in fragments {
            let object_oid = ObjectHash::new(fragment_id.as_bytes());
            let fragment_digest = hex::encode(Sha256::digest(text.as_bytes()));
            let evidence = EvidenceRefV1 {
                schema_version: 1,
                source_plane: EvidenceSourcePlane::AgentRuntime,
                kind: EvidenceKind::Task,
                object_id: fragment_id.to_string(),
                source_ref_oid: source_ref_oid.clone(),
                locator: EvidenceLocatorV1::Object,
                fragment_digest: fragment_digest.clone(),
                visibility: EvidenceVisibility::RepoLocal,
                captured_at: None,
                code_commit: None,
            };
            manifest_fragments.push(SourceManifestFragmentV1 {
                fragment_id: fragment_id.to_string(),
                object_type: object_type.to_string(),
                object_id: fragment_id.to_string(),
                object_oid: object_oid.to_string(),
                source_ref_oid: source_ref_oid.clone(),
                locator: EvidenceLocatorV1::Object,
                fragment_digest,
                code_commit: None,
            });
            redacted_fragments.push(RedactedEpisodeFragment {
                fragment_id: fragment_id.to_string(),
                object_type: object_type.to_string(),
                object_id: fragment_id.to_string(),
                object_oid,
                text: text.to_string(),
                evidence,
            });
        }
        let redacted_bytes = fragments.iter().map(|(_, _, text)| text.len()).sum();
        let token_estimate = estimate_tokens(redacted_bytes);
        let started_at = DateTime::<Utc>::from_timestamp(1_700_000_000, 0)
            .expect("fixed compiler test timestamp is valid");
        let ended_at = DateTime::<Utc>::from_timestamp(1_700_000_001, 0)
            .expect("fixed compiler test timestamp is valid");
        Self {
            manifest: EpisodeSourceManifestV1 {
                schema_version: SOURCE_SCHEMA_VERSION,
                policy_version: SOURCE_POLICY_VERSION.to_string(),
                redaction_policy_version: REDACTION_POLICY_VERSION.to_string(),
                root_kind,
                root_id: "compiler-test-root".to_string(),
                repository_id: "compiler-test-repository".to_string(),
                principal_digest: "hmac-sha256:test".to_string(),
                source_ref_oid,
                dependency_ref_oid: None,
                limits: limits.into(),
                object_count: redacted_fragments.len(),
                redacted_bytes,
                token_estimate,
                fragments: manifest_fragments,
                omissions: Vec::new(),
            },
            facts: EpisodeSourceFacts {
                related_intent_ids: Vec::new(),
                related_task_ids: vec!["compiler-test-root".to_string()],
                related_run_ids: Vec::new(),
                root_goal: "compile a bounded task episode".to_string(),
                started_at,
                ended_at,
                completion_status,
                code_change_status,
                code: EpisodeCodeContextV1 {
                    base_oid: None,
                    result_oid: None,
                    branch_ref: None,
                    paths: Vec::new(),
                },
            },
            fragments: redacted_fragments,
            pinned_task_episodes: Vec::new(),
        }
    }
}

pub(super) struct MemorySourceRedactor {
    secrets: Redactor,
    email: Regex,
    home_path: Regex,
}

impl MemorySourceRedactor {
    pub(super) fn new() -> Result<Self, EpisodeSourceError> {
        Ok(Self {
            secrets: Redactor::new_default(),
            email: Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b")
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::RedactionFailed))?,
            home_path: Regex::new(r#"/(?:Users|home)/[^/\s\"']+"#)
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::RedactionFailed))?,
        })
    }

    pub(super) fn redact(&self, raw: &[u8]) -> Result<Vec<u8>, EpisodeSourceError> {
        let (secret_redacted, _) = self.secrets.redact(raw);
        let private_redacted = redact_private_markers(secret_redacted.bytes())?;
        let email_redacted = self
            .email
            .replace_all(&private_redacted, b"<REDACTED:email>".as_slice());
        Ok(self
            .home_path
            .replace_all(&email_redacted, b"/<REDACTED:home>".as_slice())
            .into_owned())
    }
}

pub(crate) struct EpisodeSourceResolver<'a> {
    history: &'a HistoryManager,
    digest: &'a RepositoryKeyedDigest,
    limits: EpisodeSourceLimits,
    redactor: MemorySourceRedactor,
}

impl<'a> EpisodeSourceResolver<'a> {
    pub(crate) fn new(
        history: &'a HistoryManager,
        digest: &'a RepositoryKeyedDigest,
        limits: EpisodeSourceLimits,
    ) -> Result<Self, EpisodeSourceError> {
        let limits = limits
            .validate()
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::InvalidRequest))?;
        Ok(Self {
            history,
            digest,
            limits,
            redactor: MemorySourceRedactor::new()?,
        })
    }

    pub(crate) async fn resolve(
        &self,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        source_ref_oid: ObjectHash,
    ) -> Result<RedactedEpisodeSource, EpisodeSourceError> {
        let mut source = self.resolve_base(context, target, source_ref_oid).await?;
        if target.root().kind() == EpisodeRootKind::Intent {
            self.attach_intent_task_episodes(target, &mut source, None)
                .await?;
        }
        Ok(source)
    }

    async fn resolve_base(
        &self,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        source_ref_oid: ObjectHash,
    ) -> Result<RedactedEpisodeSource, EpisodeSourceError> {
        if context.repository_id() != self.digest.repository_id() {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::Unauthorized,
            ));
        }
        let view = self
            .history
            .pin_history(
                source_ref_oid,
                self.limits.max_ancestry_commits,
                self.limits.max_tree_bytes,
            )
            .await
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable))?;
        let principal_digest = self
            .digest
            .principal_digest(context.actor().principal_id.as_bytes())
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::DigestUnavailable))?
            .encoded();
        let mut collector = SourceCollector::new(
            self,
            view,
            target,
            context.repository_id(),
            principal_digest,
        );
        collector.collect()?;
        collector.finish()
    }

    pub(crate) async fn revalidate(
        &self,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        source: &RedactedEpisodeSource,
    ) -> Result<(), EpisodeSourceError> {
        let source_oid = source
            .manifest
            .source_ref_oid
            .parse::<ObjectHash>()
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        let mut rebuilt = self.resolve_base(context, target, source_oid).await?;
        if target.root().kind() == EpisodeRootKind::Intent {
            let dependency_ref_oid = source
                .manifest
                .dependency_ref_oid
                .as_deref()
                .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?
                .parse::<ObjectHash>()
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            self.attach_intent_task_episodes(
                target,
                &mut rebuilt,
                Some((dependency_ref_oid, &source.pinned_task_episodes)),
            )
            .await?;
        }
        if rebuilt.manifest != source.manifest
            || rebuilt.facts != source.facts
            || rebuilt.fragments != source.fragments
            || rebuilt.pinned_task_episodes != source.pinned_task_episodes
        {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::SourceCorrupt,
            ));
        }
        Ok(())
    }

    async fn attach_intent_task_episodes(
        &self,
        target: &TrustedMemoryTarget,
        source: &mut RedactedEpisodeSource,
        pinned: Option<(ObjectHash, &[PinnedTaskEpisodeV1])>,
    ) -> Result<(), EpisodeSourceError> {
        if source.facts.related_task_ids.is_empty()
            || source.facts.related_task_ids.len() > MAX_INTENT_TASKS
        {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::DependencyPending,
            ));
        }
        let database = self.history.database_connection();
        let memory_head = match pinned {
            Some((head, _)) => head,
            None => read_memory_ref_head(&database)
                .await
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable))?
                .ok_or_else(|| {
                    EpisodeSourceError::new(EpisodeSourceErrorKind::DependencyPending)
                })?,
        };
        let snapshot = load_snapshot(
            self.history.repository_path(),
            Some(memory_head),
            super::policy::REPO_EPISODE_POLICY_VERSION,
        )
        .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        match pinned {
            None => validate_projection_watermark(
                &database,
                Some(memory_head),
                snapshot.manifest.last_event_seq,
            )
            .await
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable))?,
            Some(_) => {
                let current_head = read_memory_ref_head(&database)
                    .await
                    .map_err(|_| {
                        EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable)
                    })?
                    .ok_or_else(|| {
                        EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt)
                    })?;
                let current_snapshot = load_snapshot(
                    self.history.repository_path(),
                    Some(current_head),
                    super::policy::REPO_EPISODE_POLICY_VERSION,
                )
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
                validate_projection_watermark(
                    &database,
                    Some(current_head),
                    current_snapshot.manifest.last_event_seq,
                )
                .await
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable))?;
                load_history_delta_bounded(
                    self.history.repository_path(),
                    current_head,
                    Some(memory_head),
                    super::policy::REPO_EPISODE_POLICY_VERSION,
                    MAX_MEMORY_DEPENDENCY_COMMITS,
                )
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            }
        }

        let mut related_run_ids = BTreeSet::new();
        let task_ids = source.facts.related_task_ids.clone();
        for task_id in task_ids {
            let task_root = super::domain::EpisodeRoot::task(task_id.clone())
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            let revision_oid = match pinned {
                Some((_, pins)) => {
                    let pin = pins
                        .iter()
                        .find(|pin| pin.task_id == task_id)
                        .ok_or_else(|| {
                            EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt)
                        })?;
                    let revision_rows = database
                        .query_all_raw(sea_orm::Statement::from_sql_and_values(
                            database.get_database_backend(),
                            "SELECT 1 AS present FROM memory_revision_index
                             WHERE scope_key = 'repo' AND namespace = 'default'
                               AND note_id = ? AND revision_oid = ? LIMIT 2",
                            [
                                task_root.note_id().to_string().into(),
                                pin.revision_oid.to_string().into(),
                            ],
                        ))
                        .await
                        .map_err(|_| {
                            EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable)
                        })?;
                    if pin.note_id != task_root.note_id() || revision_rows.len() != 1 {
                        return Err(EpisodeSourceError::new(
                            EpisodeSourceErrorKind::SourceCorrupt,
                        ));
                    }
                    pin.revision_oid
                }
                None => {
                    let rows = database
                        .query_all_raw(sea_orm::Statement::from_sql_and_values(
                            database.get_database_backend(),
                            "SELECT live_revision_oid FROM memory_head
                             WHERE scope_key = 'repo' AND namespace = 'default' AND note_id = ?
                             LIMIT 2",
                            [task_root.note_id().to_string().into()],
                        ))
                        .await
                        .map_err(|_| {
                            EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable)
                        })?;
                    if rows.len() != 1 {
                        return Err(EpisodeSourceError::new(if rows.is_empty() {
                            EpisodeSourceErrorKind::DependencyPending
                        } else {
                            EpisodeSourceErrorKind::SourceCorrupt
                        }));
                    }
                    let revision_oid: Option<String> =
                        rows[0].try_get("", "live_revision_oid").map_err(|_| {
                            EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt)
                        })?;
                    let revision_oid = revision_oid.ok_or_else(|| {
                        EpisodeSourceError::new(EpisodeSourceErrorKind::DependencyPending)
                    })?;
                    ObjectHash::from_str(&revision_oid).map_err(|_| {
                        EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt)
                    })?
                }
            };
            let note_bytes = load_note_bytes(self.history.repository_path(), revision_oid)
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            let note = parse_memory_note_v1(&note_bytes)
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            validate_task_episode_dependency(target, &task_root, &note)?;
            let episode = note
                .episode
                .as_ref()
                .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            related_run_ids.extend(episode.related_run_ids.iter().cloned());
            self.append_task_episode_fragment(source, memory_head, revision_oid, &task_id, &note)?;
        }
        if pinned.is_none()
            && read_memory_ref_head(&database)
                .await
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceNotReachable))?
                != Some(memory_head)
        {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::SourceNotReachable,
            ));
        }
        source.facts.related_run_ids = related_run_ids.into_iter().collect();
        source.manifest.dependency_ref_oid = Some(memory_head.to_string());
        Ok(())
    }

    fn append_task_episode_fragment(
        &self,
        source: &mut RedactedEpisodeSource,
        memory_head: ObjectHash,
        revision_oid: ObjectHash,
        task_id: &str,
        note: &MemoryNoteV1,
    ) -> Result<(), EpisodeSourceError> {
        if source.fragments.len() == self.limits.max_objects {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::LimitExceeded,
            ));
        }
        let compact = CompactTaskEpisodeV1::from_note(task_id, note)?;
        let raw = serde_json::to_vec(&compact)
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        let redacted = self.redactor.redact(&raw)?;
        let text = String::from_utf8(redacted)
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::RedactionFailed))?;
        let next_bytes = source.manifest.redacted_bytes.saturating_add(text.len());
        if next_bytes > self.limits.max_total_bytes
            || estimate_tokens(next_bytes) > self.limits.max_token_estimate
        {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::LimitExceeded,
            ));
        }
        let fragment_id = format!("{TASK_EPISODE}:{task_id}");
        let fragment_digest = format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())));
        let evidence = EvidenceRefV1 {
            schema_version: 1,
            source_plane: EvidenceSourcePlane::AgentRuntime,
            kind: EvidenceKind::Task,
            object_id: note.note_id.to_string(),
            source_ref_oid: memory_head.to_string(),
            locator: EvidenceLocatorV1::Object,
            fragment_digest: fragment_digest.clone(),
            visibility: EvidenceVisibility::RepoLocal,
            captured_at: Some(note.created_at),
            code_commit: note.effective_from_commit.clone(),
        };
        source.manifest.fragments.push(SourceManifestFragmentV1 {
            fragment_id: fragment_id.clone(),
            object_type: TASK_EPISODE.to_string(),
            object_id: task_id.to_string(),
            object_oid: revision_oid.to_string(),
            source_ref_oid: memory_head.to_string(),
            locator: EvidenceLocatorV1::Object,
            fragment_digest,
            code_commit: note.effective_from_commit.clone(),
        });
        source.fragments.push(RedactedEpisodeFragment {
            fragment_id: fragment_id.clone(),
            object_type: TASK_EPISODE.to_string(),
            object_id: task_id.to_string(),
            object_oid: revision_oid,
            text,
            evidence,
        });
        source.pinned_task_episodes.push(PinnedTaskEpisodeV1 {
            task_id: task_id.to_string(),
            note_id: note.note_id,
            revision_oid,
            fragment_id,
        });
        source.manifest.object_count = source.fragments.len();
        source.manifest.redacted_bytes = next_bytes;
        source.manifest.token_estimate = estimate_tokens(next_bytes);
        Ok(())
    }
}

#[derive(Serialize)]
struct CompactTaskEpisodeV1<'a> {
    schema_version: u32,
    task_id: &'a str,
    completion_status: CompletionStatus,
    code_change_status: CodeChangeStatus,
    summary: CompactEpisodeClaimV1<'a>,
    observations: Vec<CompactEpisodeClaimV1<'a>>,
    inferences: Vec<CompactEpisodeClaimV1<'a>>,
    decisions: Vec<CompactEpisodeClaimV1<'a>>,
    failed_attempts: Vec<CompactEpisodeClaimV1<'a>>,
    unresolved: Vec<CompactEpisodeClaimV1<'a>>,
    related_run_ids: &'a [String],
}

impl<'a> CompactTaskEpisodeV1<'a> {
    fn from_note(task_id: &'a str, note: &'a MemoryNoteV1) -> Result<Self, EpisodeSourceError> {
        let episode = note
            .episode
            .as_ref()
            .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        Ok(Self {
            schema_version: 1,
            task_id,
            completion_status: episode.completion_status,
            code_change_status: episode.code_change_status,
            summary: CompactEpisodeClaimV1::from(&episode.summary),
            observations: compact_claims(&episode.observations),
            inferences: compact_claims(&episode.inferences),
            decisions: compact_claims(&episode.decisions),
            failed_attempts: compact_claims(&episode.failed_attempts),
            unresolved: compact_claims(&episode.unresolved),
            related_run_ids: &episode.related_run_ids,
        })
    }
}

pub(super) fn compact_task_episode_bytes(
    task_id: &str,
    note: &MemoryNoteV1,
) -> Result<Vec<u8>, EpisodeSourceError> {
    let compact = CompactTaskEpisodeV1::from_note(task_id, note)?;
    serde_json::to_vec(&compact)
        .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))
}

#[derive(Serialize)]
struct CompactEpisodeClaimV1<'a> {
    epistemic_status: super::domain::EpistemicStatus,
    claim: &'a str,
    confidence: Option<crate::internal::ai::context_budget::MemoryAnchorConfidence>,
}

impl<'a> From<&'a super::domain::EpisodeClaimV1> for CompactEpisodeClaimV1<'a> {
    fn from(claim: &'a super::domain::EpisodeClaimV1) -> Self {
        Self {
            epistemic_status: claim.epistemic_status,
            claim: &claim.claim,
            confidence: claim.confidence,
        }
    }
}

fn compact_claims(claims: &[super::domain::EpisodeClaimV1]) -> Vec<CompactEpisodeClaimV1<'_>> {
    claims.iter().map(Into::into).collect()
}

fn validate_task_episode_dependency(
    intent_target: &TrustedMemoryTarget,
    task_root: &super::domain::EpisodeRoot,
    note: &MemoryNoteV1,
) -> Result<(), EpisodeSourceError> {
    let episode = note
        .episode
        .as_ref()
        .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
    if note.note_id != task_root.note_id()
        || note.namespace != task_root.namespace()
        || note.path != task_root.path()
        || episode.root_kind != EpisodeRootKind::Task
        || episode.root_id != task_root.id()
        || !episode
            .related_task_ids
            .iter()
            .any(|task_id| task_id == task_root.id())
        || !episode
            .related_intent_ids
            .iter()
            .any(|intent_id| intent_id == intent_target.root().id())
    {
        return Err(EpisodeSourceError::new(
            EpisodeSourceErrorKind::SourceCorrupt,
        ));
    }
    Ok(())
}

struct SourceCollector<'resolver, 'history> {
    resolver: &'resolver EpisodeSourceResolver<'history>,
    view: PinnedHistoryView<'history>,
    target: &'resolver TrustedMemoryTarget,
    repository_id: &'resolver str,
    principal_digest: String,
    fragments: Vec<RedactedEpisodeFragment>,
    values: BTreeMap<(String, String), Value>,
    omissions: BTreeMap<(String, String), usize>,
    candidate_count: usize,
    redacted_bytes: usize,
    context_fragments: usize,
    intent_ids: BTreeSet<String>,
    task_ids: BTreeSet<String>,
    run_ids: BTreeSet<String>,
}

impl<'resolver, 'history> SourceCollector<'resolver, 'history> {
    fn new(
        resolver: &'resolver EpisodeSourceResolver<'history>,
        view: PinnedHistoryView<'history>,
        target: &'resolver TrustedMemoryTarget,
        repository_id: &'resolver str,
        principal_digest: String,
    ) -> Self {
        let mut intent_ids = BTreeSet::new();
        let mut task_ids = BTreeSet::new();
        match target.root().kind() {
            EpisodeRootKind::Task => {
                task_ids.insert(target.root().id().to_string());
            }
            EpisodeRootKind::Intent => {
                intent_ids.insert(target.root().id().to_string());
            }
        }
        Self {
            resolver,
            view,
            target,
            repository_id,
            principal_digest,
            fragments: Vec::new(),
            values: BTreeMap::new(),
            omissions: BTreeMap::new(),
            candidate_count: 0,
            redacted_bytes: 0,
            context_fragments: 0,
            intent_ids,
            task_ids,
            run_ids: BTreeSet::new(),
        }
    }

    fn collect(&mut self) -> Result<(), EpisodeSourceError> {
        let root_type = match self.target.root().kind() {
            EpisodeRootKind::Task => TASK,
            EpisodeRootKind::Intent => INTENT,
        };
        let root_id = self.target.root().id().to_string();
        let root = self
            .load_exact(root_type, &root_id)?
            .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        self.include(root_type, root, true)?;

        match self.target.root().kind() {
            EpisodeRootKind::Task => {
                let root_id = root_id.clone();
                self.scan_with_requirement(
                    TASK_EVENT,
                    move |value| field_id(value, "task_id") == Some(root_id.as_str()),
                    |value| {
                        matches!(
                            field_id(value, "kind"),
                            Some("done" | "failed" | "cancelled")
                        )
                    },
                )?;
            }
            EpisodeRootKind::Intent => {
                let root_id = root_id.clone();
                self.scan_with_requirement(
                    INTENT_EVENT,
                    move |value| field_id(value, "intent_id") == Some(root_id.as_str()),
                    |value| matches!(field_id(value, "kind"), Some("completed" | "cancelled")),
                )?;
            }
        }

        if root_type == TASK {
            let linked_intent = self
                .value(TASK, &root_id)?
                .get("intent")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(intent_id) = linked_intent {
                self.intent_ids.insert(intent_id.clone());
                if let Some(intent) = self.load_exact(INTENT, &intent_id)? {
                    self.include(INTENT, intent, false)?;
                }
            }
        }
        if root_type == INTENT {
            self.collect_intent_task_ids(&root_id)?;
            return Ok(());
        }

        let task_ids = self.task_ids.clone();
        self.scan(RUN, move |value| {
            field_id(value, "task").is_some_and(|id| task_ids.contains(id))
        })?;
        self.run_ids.extend(
            self.values
                .keys()
                .filter(|(kind, _)| kind == RUN)
                .map(|(_, id)| id.clone()),
        );

        match self.target.root().kind() {
            EpisodeRootKind::Task => {
                let intent_ids = self.intent_ids.clone();
                self.scan(INTENT_EVENT, move |value| {
                    field_id(value, "intent_id").is_some_and(|id| intent_ids.contains(id))
                })?;
            }
            EpisodeRootKind::Intent => {
                let task_ids = self.task_ids.clone();
                self.scan(TASK_EVENT, move |value| {
                    field_id(value, "task_id").is_some_and(|id| task_ids.contains(id))
                })?;
            }
        }
        for object_type in [RUN_EVENT, EVIDENCE, DECISION, PATCHSET, INVOCATION] {
            let run_ids = self.run_ids.clone();
            self.scan(object_type, move |value| {
                relation_run_id(value).is_some_and(|id| run_ids.contains(id))
            })?;
        }
        let run_ids = self.run_ids.clone();
        let intent_ids = self.intent_ids.clone();
        self.scan(CONTEXT_FRAME, move |value| {
            field_id(value, "run_id").is_some_and(|id| run_ids.contains(id))
                || field_id(value, "intent_id").is_some_and(|id| intent_ids.contains(id))
        })?;
        Ok(())
    }

    fn finish(self) -> Result<RedactedEpisodeSource, EpisodeSourceError> {
        let root_type = match self.target.root().kind() {
            EpisodeRootKind::Task => TASK,
            EpisodeRootKind::Intent => INTENT,
        };
        let redacted_root = self
            .fragments
            .iter()
            .find(|fragment| {
                fragment.object_type == root_type && fragment.object_id == self.target.root().id()
            })
            .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        let redacted_root_value: Value = serde_json::from_str(redacted_root.text())
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        let root_goal = match self.target.root().kind() {
            EpisodeRootKind::Task => redacted_root_value
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| {
                    redacted_root_value
                        .get("description")
                        .and_then(Value::as_str)
                })
                .unwrap_or("task"),
            EpisodeRootKind::Intent => redacted_root_value
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("intent"),
        }
        .to_string();
        let root_value = self.value(root_type, self.target.root().id())?;
        let started_at = parse_timestamp(root_value, "created_at")?;
        let (completion_status, ended_at) = terminal_fact(
            &self.values,
            self.target.root().kind(),
            self.target.root().id(),
        )?;
        let code = derive_code_context(&self.values, &self.run_ids);
        let code_change_status = match (&code.base_oid, &code.result_oid) {
            (_, None) => CodeChangeStatus::Unknown,
            (Some(base), Some(result)) if base == result => CodeChangeStatus::Unchanged,
            (_, Some(_)) => CodeChangeStatus::Changed,
        };
        let omissions = self
            .omissions
            .into_iter()
            .map(|((code, object_type), count)| SourceOmissionV1 {
                code,
                object_type,
                count,
            })
            .collect::<Vec<_>>();
        let token_estimate = estimate_tokens(self.redacted_bytes);
        let manifest = EpisodeSourceManifestV1 {
            schema_version: SOURCE_SCHEMA_VERSION,
            policy_version: SOURCE_POLICY_VERSION.to_string(),
            redaction_policy_version: REDACTION_POLICY_VERSION.to_string(),
            root_kind: self.target.root().kind(),
            root_id: self.target.root().id().to_string(),
            repository_id: self.repository_id.to_string(),
            principal_digest: self.principal_digest,
            source_ref_oid: self.view.head().to_string(),
            dependency_ref_oid: None,
            limits: self.resolver.limits.into(),
            object_count: self.fragments.len(),
            redacted_bytes: self.redacted_bytes,
            token_estimate,
            fragments: self
                .fragments
                .iter()
                .map(|fragment| SourceManifestFragmentV1 {
                    fragment_id: fragment.fragment_id.clone(),
                    object_type: fragment.object_type.clone(),
                    object_id: fragment.object_id.clone(),
                    object_oid: fragment.object_oid.to_string(),
                    source_ref_oid: fragment.evidence.source_ref_oid.clone(),
                    locator: fragment.evidence.locator.clone(),
                    fragment_digest: fragment.evidence.fragment_digest.clone(),
                    code_commit: fragment.evidence.code_commit.clone(),
                })
                .collect(),
            omissions,
        };
        Ok(RedactedEpisodeSource {
            manifest,
            facts: EpisodeSourceFacts {
                related_intent_ids: self.intent_ids.into_iter().collect(),
                related_task_ids: self.task_ids.into_iter().collect(),
                related_run_ids: self.run_ids.into_iter().collect(),
                root_goal,
                started_at,
                ended_at,
                completion_status,
                code_change_status,
                code,
            },
            fragments: self.fragments,
            pinned_task_episodes: Vec::new(),
        })
    }

    fn value(&self, object_type: &str, object_id: &str) -> Result<&Value, EpisodeSourceError> {
        self.values
            .get(&(object_type.to_string(), object_id.to_string()))
            .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))
    }

    fn load_exact(
        &mut self,
        object_type: &str,
        object_id: &str,
    ) -> Result<Option<PinnedHistoryBlob>, EpisodeSourceError> {
        self.consume_candidate()?;
        self.view
            .get_blob(
                object_type,
                object_id,
                self.resolver.limits.max_object_bytes,
            )
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))
    }

    fn scan<F>(&mut self, object_type: &str, predicate: F) -> Result<(), EpisodeSourceError>
    where
        F: Fn(&Value) -> bool,
    {
        self.scan_with_requirement(object_type, predicate, |_| false)
    }

    fn collect_intent_task_ids(&mut self, intent_id: &str) -> Result<(), EpisodeSourceError> {
        let remaining = self
            .resolver
            .limits
            .max_candidate_objects
            .saturating_sub(self.candidate_count);
        if remaining == 0 {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::LimitExceeded,
            ));
        }
        let listing = self
            .view
            .list(TASK, remaining)
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        if listing.omitted() != 0 {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::LimitExceeded,
            ));
        }
        let entries = listing.entries().to_vec();
        for entry in entries {
            self.consume_candidate()?;
            let blob = self
                .view
                .read_blob(&entry, self.resolver.limits.max_object_bytes)
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            let value = parse_object(TASK, &blob)?;
            if field_id(&value, "intent") == Some(intent_id) {
                self.task_ids.insert(blob.object_id().to_string());
                if self.task_ids.len() > MAX_INTENT_TASKS {
                    return Err(EpisodeSourceError::new(
                        EpisodeSourceErrorKind::LimitExceeded,
                    ));
                }
            }
        }
        Ok(())
    }

    fn scan_with_requirement<F, R>(
        &mut self,
        object_type: &str,
        predicate: F,
        required: R,
    ) -> Result<(), EpisodeSourceError>
    where
        F: Fn(&Value) -> bool,
        R: Fn(&Value) -> bool,
    {
        let remaining = self
            .resolver
            .limits
            .max_candidate_objects
            .saturating_sub(self.candidate_count);
        if remaining == 0 {
            self.omit("candidate_limit", object_type, 1);
            return Ok(());
        }
        let listing = self
            .view
            .list(object_type, remaining)
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
        if listing.omitted() > 0 {
            self.omit("candidate_limit", object_type, listing.omitted());
        }
        let entries = listing.entries().to_vec();
        for entry in entries {
            self.consume_candidate()?;
            let blob = self
                .view
                .read_blob(&entry, self.resolver.limits.max_object_bytes)
                .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
            let value = parse_object(object_type, &blob)?;
            if predicate(&value) {
                let is_required = required(&value);
                self.include_parsed(object_type, blob, value, is_required)?;
            }
        }
        Ok(())
    }

    fn consume_candidate(&mut self) -> Result<(), EpisodeSourceError> {
        self.candidate_count = self.candidate_count.saturating_add(1);
        if self.candidate_count > self.resolver.limits.max_candidate_objects {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::LimitExceeded,
            ));
        }
        Ok(())
    }

    fn include(
        &mut self,
        object_type: &str,
        blob: PinnedHistoryBlob,
        required: bool,
    ) -> Result<(), EpisodeSourceError> {
        let value = parse_object(object_type, &blob)?;
        self.include_parsed(object_type, blob, value, required)
    }

    fn include_parsed(
        &mut self,
        object_type: &str,
        blob: PinnedHistoryBlob,
        value: Value,
        required: bool,
    ) -> Result<(), EpisodeSourceError> {
        let key = (object_type.to_string(), blob.object_id().to_string());
        if self.values.contains_key(&key) {
            return Ok(());
        }
        if self.fragments.len() == self.resolver.limits.max_objects {
            if required {
                return Err(EpisodeSourceError::new(
                    EpisodeSourceErrorKind::LimitExceeded,
                ));
            }
            self.omit("object_limit", object_type, 1);
            return Ok(());
        }
        if object_type == CONTEXT_FRAME {
            if self.context_fragments == self.resolver.limits.max_context_fragments {
                self.omit("context_fragment_limit", object_type, 1);
                return Ok(());
            }
            self.context_fragments += 1;
        }
        let redacted = self.resolver.redactor.redact(blob.bytes())?;
        let next_bytes = self.redacted_bytes.saturating_add(redacted.len());
        if next_bytes > self.resolver.limits.max_total_bytes
            || estimate_tokens(next_bytes) > self.resolver.limits.max_token_estimate
        {
            if required {
                return Err(EpisodeSourceError::new(
                    EpisodeSourceErrorKind::LimitExceeded,
                ));
            }
            self.omit("source_budget", object_type, 1);
            return Ok(());
        }
        let text = String::from_utf8(redacted)
            .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::RedactionFailed))?;
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())));
        let fragment_id = format!("{object_type}:{}", blob.object_id());
        let evidence = EvidenceRefV1 {
            schema_version: 1,
            source_plane: EvidenceSourcePlane::AgentRuntime,
            kind: evidence_kind(object_type),
            object_id: blob.object_id().to_string(),
            source_ref_oid: self.view.head().to_string(),
            locator: evidence_locator(object_type, blob.object_id()),
            fragment_digest: digest,
            visibility: EvidenceVisibility::RepoLocal,
            captured_at: value
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|timestamp| timestamp.parse().ok()),
            code_commit: code_commit(&value),
        };
        self.redacted_bytes = next_bytes;
        self.values.insert(key, value);
        self.fragments.push(RedactedEpisodeFragment {
            fragment_id,
            object_type: object_type.to_string(),
            object_id: blob.object_id().to_string(),
            object_oid: blob.oid(),
            text,
            evidence,
        });
        Ok(())
    }

    fn omit(&mut self, code: &str, object_type: &str, count: usize) {
        *self
            .omissions
            .entry((code.to_string(), object_type.to_string()))
            .or_default() += count;
    }
}

fn parse_object(object_type: &str, blob: &PinnedHistoryBlob) -> Result<Value, EpisodeSourceError> {
    let value: Value = serde_json::from_slice(blob.bytes())
        .map_err(|_| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))?;
    if field_id(&value, "object_id") != Some(blob.object_id())
        || value.get("object_type").and_then(Value::as_str) != Some(object_type)
    {
        return Err(EpisodeSourceError::new(
            EpisodeSourceErrorKind::SourceCorrupt,
        ));
    }
    Ok(value)
}

fn field_id<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn relation_run_id(value: &Value) -> Option<&str> {
    field_id(value, "run_id").or_else(|| field_id(value, "run"))
}

fn code_commit(value: &Value) -> Option<String> {
    value
        .get("commit")
        .and_then(|commit| {
            commit
                .as_str()
                .or_else(|| commit.get("value").and_then(Value::as_str))
        })
        .map(str::to_string)
        .or_else(|| field_id(value, "result_commit_sha").map(str::to_string))
}

fn parse_timestamp(value: &Value, field: &str) -> Result<DateTime<Utc>, EpisodeSourceError> {
    field_id(value, field)
        .and_then(|timestamp| timestamp.parse().ok())
        .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::SourceCorrupt))
}

fn terminal_fact(
    values: &BTreeMap<(String, String), Value>,
    root_kind: EpisodeRootKind,
    root_id: &str,
) -> Result<(CompletionStatus, DateTime<Utc>), EpisodeSourceError> {
    let (event_type, root_field) = match root_kind {
        EpisodeRootKind::Task => (TASK_EVENT, "task_id"),
        EpisodeRootKind::Intent => (INTENT_EVENT, "intent_id"),
    };
    let mut terminal = values
        .iter()
        .filter(|((object_type, _), value)| {
            object_type == event_type && field_id(value, root_field) == Some(root_id)
        })
        .filter_map(|((_, object_id), value)| {
            let status = match (root_kind, field_id(value, "kind")?) {
                (EpisodeRootKind::Task, "done") | (EpisodeRootKind::Intent, "completed") => {
                    CompletionStatus::Completed
                }
                (EpisodeRootKind::Task, "failed") => CompletionStatus::Failed,
                (EpisodeRootKind::Task, "cancelled") | (EpisodeRootKind::Intent, "cancelled") => {
                    CompletionStatus::Cancelled
                }
                _ => return None,
            };
            let at = parse_timestamp(value, "created_at").ok()?;
            Some((at, object_id.clone(), status))
        })
        .collect::<Vec<_>>();
    terminal.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    terminal
        .pop()
        .map(|(at, _, status)| (status, at))
        .ok_or_else(|| EpisodeSourceError::new(EpisodeSourceErrorKind::InvalidRequest))
}

fn derive_code_context(
    values: &BTreeMap<(String, String), Value>,
    run_ids: &BTreeSet<String>,
) -> EpisodeCodeContextV1 {
    let mut run_commits = values
        .iter()
        .filter(|((object_type, object_id), _)| object_type == RUN && run_ids.contains(object_id))
        .filter_map(|((_, object_id), value)| {
            Some((
                parse_timestamp(value, "created_at").ok()?,
                object_id,
                code_commit(value)?,
            ))
        })
        .collect::<Vec<_>>();
    run_commits.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    let base_oid = run_commits.first().map(|(_, _, commit)| commit.clone());

    let mut result_commits = values
        .iter()
        .filter_map(|((object_type, object_id), value)| {
            if object_type != DECISION && object_type != INTENT_EVENT {
                return None;
            }
            let result = field_id(value, "result_commit_sha")
                .or_else(|| field_id(value, "result_commit"))?;
            Some((
                parse_timestamp(value, "created_at").ok()?,
                object_id,
                result.to_string(),
            ))
        })
        .collect::<Vec<_>>();
    result_commits.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    let result_oid = result_commits.pop().map(|(_, _, commit)| commit);
    let paths = values
        .iter()
        .filter(|((object_type, _), _)| object_type == PATCHSET)
        .flat_map(|(_, value)| {
            value
                .get("touched")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|entry| field_id(entry, "path").map(str::to_string))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    EpisodeCodeContextV1 {
        base_oid,
        result_oid,
        branch_ref: None,
        paths,
    }
}

fn evidence_kind(object_type: &str) -> EvidenceKind {
    match object_type {
        INTENT | INTENT_EVENT => EvidenceKind::Intent,
        TASK | TASK_EVENT => EvidenceKind::Task,
        RUN | RUN_EVENT => EvidenceKind::Run,
        EVIDENCE => EvidenceKind::Evidence,
        DECISION => EvidenceKind::Decision,
        PATCHSET => EvidenceKind::PatchSet,
        CONTEXT_FRAME => EvidenceKind::Evidence,
        INVOCATION => EvidenceKind::ToolCall,
        _ => EvidenceKind::Evidence,
    }
}

fn evidence_locator(object_type: &str, object_id: &str) -> EvidenceLocatorV1 {
    if object_type == INVOCATION {
        EvidenceLocatorV1::ToolCall {
            invocation_id: object_id.to_string(),
            part: ToolCallPart::Invocation,
        }
    } else {
        EvidenceLocatorV1::Object
    }
}

fn estimate_tokens(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

fn redact_private_markers(input: &[u8]) -> Result<Vec<u8>, EpisodeSourceError> {
    const OPEN: &[u8] = b"<private>";
    const CLOSE: &[u8] = b"</private>";
    const REPLACEMENT: &[u8] = b"<REDACTED:private-marker>";

    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative_open) = find_bytes(&input[cursor..], OPEN) {
        let open = cursor + relative_open;
        output.extend_from_slice(&input[cursor..open]);
        let body_start = open + OPEN.len();
        let Some(relative_close) = find_bytes(&input[body_start..], CLOSE) else {
            return Err(EpisodeSourceError::new(
                EpisodeSourceErrorKind::RedactionFailed,
            ));
        };
        output.extend_from_slice(REPLACEMENT);
        cursor = body_start + relative_close + CLOSE.len();
    }
    if find_bytes(&input[cursor..], CLOSE).is_some() {
        return Err(EpisodeSourceError::new(
            EpisodeSourceErrorKind::RedactionFailed,
        ));
    }
    output.extend_from_slice(&input[cursor..]);
    Ok(output)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use git_internal::internal::object::{
        context_frame::{ContextFrame, FrameKind},
        intent::Intent,
        run::Run,
        task::Task,
        task_event::{TaskEvent, TaskEventKind},
        types::{ActorRef, ObjectType},
    };
    use sea_orm::DatabaseConnection;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::{
        internal::{
            ai::{
                context_budget::MemoryAnchorConfidence,
                history::HistoryManager,
                keyed_digest::RepositoryKeyedDigest,
                memory::{
                    admission::{EpisodeAdmission, EpisodeAdmissionErrorKind},
                    compiler::{
                        EpisodeClaimProposalV1, EpisodeCompileConfig, EpisodeCompiler,
                        EpisodeCompilerError, EpisodeCompilerProposalV1,
                    },
                    domain::{ActorKind, ActorRefV1, EpisodeRoot, EpistemicStatus},
                    writer::MemoryWriter,
                },
            },
            config::ConfigKv,
            db,
        },
        utils::{storage::local::LocalStorage, storage_ext::StorageExt},
    };

    const REPOSITORY_ID: &str = "source-test-repository";
    const SECRET: &str = "github_pat_abcdefghijklmnopqrstuvwxyz1234567890";

    struct Fixture {
        _temp: TempDir,
        history: HistoryManager,
        database: Arc<DatabaseConnection>,
        digest: Arc<RepositoryKeyedDigest>,
        context: AuthenticatedMemoryContext,
        target: TrustedMemoryTarget,
        source_head: ObjectHash,
        before_task_head: ObjectHash,
        unrelated_task_id: String,
    }

    struct FakeCompiler {
        evidence_fragment_id: String,
    }

    #[async_trait::async_trait]
    impl EpisodeCompiler for FakeCompiler {
        async fn compile(
            &self,
            _source: &RedactedEpisodeSource,
            _config: &EpisodeCompileConfig,
        ) -> Result<EpisodeCompilerProposalV1, EpisodeCompilerError> {
            let observation = EpisodeClaimProposalV1 {
                epistemic_status: EpistemicStatus::Observation,
                claim: "the focused test failed before retry".to_string(),
                confidence: None,
                evidence_fragment_ids: vec![self.evidence_fragment_id.clone()],
            };
            let inference = EpisodeClaimProposalV1 {
                epistemic_status: EpistemicStatus::Inference,
                claim: "retry timing caused the failure".to_string(),
                confidence: Some(MemoryAnchorConfidence::Low),
                evidence_fragment_ids: vec![self.evidence_fragment_id.clone()],
            };
            Ok(EpisodeCompilerProposalV1 {
                summary: inference.clone(),
                observations: vec![observation],
                inferences: vec![inference],
                decisions: Vec::new(),
                failed_attempts: Vec::new(),
                unresolved: Vec::new(),
            })
        }
    }

    async fn fixture() -> Fixture {
        fixture_with_terminal(TaskEventKind::Done).await
    }

    async fn fixture_with_terminal(terminal_kind: TaskEventKind) -> Fixture {
        let temp = tempfile::tempdir().expect("create source repository");
        let database: DatabaseConnection = db::create_database(
            temp.path()
                .join("libra.db")
                .to_str()
                .expect("temporary path must be UTF-8"),
        )
        .await
        .expect("create source database");
        ConfigKv::set_with_conn(&database, "libra.repoid", REPOSITORY_ID, false)
            .await
            .expect("persist repository identity");
        ConfigKv::set_with_conn(
            &database,
            "memory.keyed_digest.v1",
            "source-test-ciphertext",
            true,
        )
        .await
        .expect("persist digest configuration fingerprint");
        let database = Arc::new(database);
        let storage = Arc::new(LocalStorage::new(temp.path().join("objects")));
        let history = HistoryManager::new(
            storage.clone(),
            temp.path().to_path_buf(),
            Arc::clone(&database),
        );
        let actor = ActorRef::agent("source-test-agent").expect("construct actor");

        let intent = Intent::new(actor.clone(), "Implement bounded memory source")
            .expect("construct intent");
        let intent_id = intent.header().object_id();
        storage
            .put_tracked(&intent, &history)
            .await
            .expect("persist intent");
        let before_task_head = history
            .resolve_history_head()
            .await
            .expect("read history head")
            .expect("intent commit must exist");

        let mut task = Task::new(
            actor.clone(),
            format!(
                "Fix retry {SECRET} alice@example.com /Users/alice/project <private>hidden note</private>"
            ),
            None,
        )
        .expect("construct task");
        task.set_intent(Some(intent_id));
        let task_id = task.header().object_id();
        storage
            .put_tracked(&task, &history)
            .await
            .expect("persist task");

        let run = Run::new(actor.clone(), task_id, "a".repeat(64)).expect("construct run");
        let run_id = run.header().object_id();
        storage
            .put_tracked(&run, &history)
            .await
            .expect("persist run");

        let mut frame = ContextFrame::new(
            actor.clone(),
            FrameKind::ErrorRecovery,
            "Focused test failed before retry",
        )
        .expect("construct context frame");
        frame.set_run_id(Some(run_id));
        storage
            .put_tracked(&frame, &history)
            .await
            .expect("persist context frame");

        let terminal =
            TaskEvent::new(actor.clone(), task_id, terminal_kind).expect("construct terminal");
        storage
            .put_tracked(&terminal, &history)
            .await
            .expect("persist task terminal event");

        let unrelated = Task::new(actor, "Unrelated task", None).expect("construct unrelated task");
        let unrelated_task_id = unrelated.header().object_id().to_string();
        storage
            .put_tracked(&unrelated, &history)
            .await
            .expect("persist unrelated task");
        let source_head = history
            .resolve_history_head()
            .await
            .expect("read source head")
            .expect("source head must exist");

        let digest = Arc::new(RepositoryKeyedDigest::for_receipt_tests(
            REPOSITORY_ID,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")
                .expect("fixed key ID must be valid"),
            [7; 32],
            "source-test-ciphertext",
        ));
        let context = AuthenticatedMemoryContext::new(
            REPOSITORY_ID,
            ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "agent:episode-compiler".to_string(),
            },
        )
        .expect("construct authenticated context");
        let target = TrustedMemoryTarget::episode(
            EpisodeRoot::task(task_id.to_string()).expect("construct task root"),
        );
        Fixture {
            _temp: temp,
            history,
            database,
            digest,
            context,
            target,
            source_head,
            before_task_head,
            unrelated_task_id,
        }
    }

    #[test]
    fn private_markers_are_removed_and_malformed_markers_fail_closed() {
        assert_eq!(
            redact_private_markers(b"before <private>secret</private> after")
                .expect("balanced marker must redact"),
            b"before <REDACTED:private-marker> after"
        );
        assert!(redact_private_markers(b"<private>secret").is_err());
        assert!(redact_private_markers(b"secret</private>").is_err());
    }

    #[test]
    fn frozen_limits_reject_zero_and_inverted_candidate_budget() {
        assert!(EpisodeSourceLimits::repo_v1().validate().is_ok());
        assert!(
            EpisodeSourceLimits {
                max_objects: 2,
                max_candidate_objects: 1,
                ..EpisodeSourceLimits::repo_v1()
            }
            .validate()
            .is_err()
        );
    }

    #[tokio::test]
    async fn task_source_is_pinned_bounded_related_and_redacted() {
        let fixture = fixture().await;
        let resolver = EpisodeSourceResolver::new(
            &fixture.history,
            &fixture.digest,
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct source resolver");
        let source = resolver
            .resolve(&fixture.context, &fixture.target, fixture.source_head)
            .await
            .expect("resolve task source");

        assert_eq!(source.manifest().root_kind, EpisodeRootKind::Task);
        assert_eq!(
            source.manifest().limits,
            EpisodeSourceLimitSnapshotV1::from(EpisodeSourceLimits::repo_v1())
        );
        assert_eq!(source.manifest().fragments.len(), source.fragments().len());
        assert!(
            source
                .manifest()
                .fragments
                .iter()
                .zip(source.fragments())
                .all(|(manifest, fragment)| {
                    manifest.fragment_id == fragment.fragment_id()
                        && manifest.object_type == fragment.object_type()
                        && manifest.object_id == fragment.object_id()
                        && manifest.object_oid == fragment.object_oid().to_string()
                        && manifest.locator == fragment.evidence().locator
                        && manifest.fragment_digest == fragment.evidence().fragment_digest
                        && manifest.code_commit == fragment.evidence().code_commit
                })
        );
        let serialized_manifest =
            serde_json::to_string(source.manifest()).expect("serialize source manifest");
        assert!(!serialized_manifest.contains(fixture.context.actor().principal_id.as_str()));
        assert!(
            source
                .manifest()
                .principal_digest
                .starts_with("hmac-sha256:")
        );
        assert!(
            source
                .fragments()
                .iter()
                .any(|fragment| fragment.object_type() == TASK)
        );
        assert!(
            source
                .fragments()
                .iter()
                .any(|fragment| fragment.object_type() == INTENT)
        );
        assert!(
            source
                .fragments()
                .iter()
                .any(|fragment| fragment.object_type() == RUN)
        );
        assert!(
            source
                .fragments()
                .iter()
                .any(|fragment| fragment.object_type() == CONTEXT_FRAME)
        );
        assert!(
            source
                .fragments()
                .iter()
                .all(|fragment| fragment.object_id() != fixture.unrelated_task_id)
        );
        let compiler_input = source
            .fragments()
            .iter()
            .map(RedactedEpisodeFragment::text)
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [SECRET, "alice@example.com", "/Users/alice", "hidden note"] {
            assert!(!compiler_input.contains(forbidden));
        }
        assert!(compiler_input.contains("<REDACTED:github-fine-grained-pat>"));
        assert!(compiler_input.contains("<REDACTED:email>"));
        assert!(compiler_input.contains("<REDACTED:private-marker>"));
        assert!(source.fragments().iter().all(|fragment| {
            fragment.evidence().source_ref_oid == fixture.source_head.to_string()
                && fragment.evidence().fragment_digest.starts_with("sha256:")
        }));
        resolver
            .revalidate(&fixture.context, &fixture.target, &source)
            .await
            .expect("revalidate exact source fragments");

        let other_context = AuthenticatedMemoryContext::new(
            REPOSITORY_ID,
            ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "agent:other-compiler".to_string(),
            },
        )
        .expect("construct other authenticated context");
        let Err(other_principal) = resolver
            .revalidate(&other_context, &fixture.target, &source)
            .await
        else {
            panic!("source manifest must remain bound to the resolving principal");
        };
        assert_eq!(
            other_principal.kind(),
            EpisodeSourceErrorKind::SourceCorrupt
        );
    }

    #[tokio::test]
    async fn source_rejects_foreign_identity_and_root_missing_at_pinned_head() {
        let fixture = fixture().await;
        let resolver = EpisodeSourceResolver::new(
            &fixture.history,
            &fixture.digest,
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct source resolver");
        let foreign_context =
            AuthenticatedMemoryContext::new("foreign-repository", fixture.context.actor().clone())
                .expect("construct foreign context");
        let Err(foreign) = resolver
            .resolve(&foreign_context, &fixture.target, fixture.source_head)
            .await
        else {
            panic!("foreign repository must be rejected");
        };
        assert_eq!(foreign.kind(), EpisodeSourceErrorKind::Unauthorized);

        let Err(missing) = resolver
            .resolve(&fixture.context, &fixture.target, fixture.before_task_head)
            .await
        else {
            panic!("future root must not be visible from an older pinned head");
        };
        assert_eq!(missing.kind(), EpisodeSourceErrorKind::SourceCorrupt);

        let task_blob = fixture
            .history
            .get_object_hash(TASK, fixture.target.root().id())
            .await
            .expect("resolve task blob")
            .expect("task blob must exist");
        let Err(not_a_history_commit) = resolver
            .resolve(&fixture.context, &fixture.target, task_blob)
            .await
        else {
            panic!("an arbitrary repository object must not be accepted as source head");
        };
        assert_eq!(
            not_a_history_commit.kind(),
            EpisodeSourceErrorKind::SourceNotReachable
        );

        let bounded_tree_resolver = EpisodeSourceResolver::new(
            &fixture.history,
            &fixture.digest,
            EpisodeSourceLimits {
                max_tree_bytes: 1,
                ..EpisodeSourceLimits::repo_v1()
            },
        )
        .expect("construct tree-bounded resolver");
        let Err(oversized_tree) = bounded_tree_resolver
            .resolve(&fixture.context, &fixture.target, fixture.source_head)
            .await
        else {
            panic!("tree reads must obey the configured byte limit");
        };
        assert_eq!(
            oversized_tree.kind(),
            EpisodeSourceErrorKind::SourceNotReachable
        );
    }

    #[tokio::test]
    async fn source_records_stable_omissions_at_object_limit() {
        let fixture = fixture().await;
        let resolver = EpisodeSourceResolver::new(
            &fixture.history,
            &fixture.digest,
            EpisodeSourceLimits {
                max_objects: 2,
                ..EpisodeSourceLimits::repo_v1()
            },
        )
        .expect("construct bounded source resolver");
        let source = resolver
            .resolve(&fixture.context, &fixture.target, fixture.source_head)
            .await
            .expect("resolve root-only source");
        assert_eq!(source.fragments().len(), 2);
        assert!(
            source
                .manifest()
                .omissions
                .iter()
                .any(|omission| omission.code == "object_limit")
        );
    }

    #[tokio::test]
    async fn source_preserves_every_task_terminal_outcome() {
        for (kind, expected) in [
            (TaskEventKind::Done, CompletionStatus::Completed),
            (TaskEventKind::Failed, CompletionStatus::Failed),
            (TaskEventKind::Cancelled, CompletionStatus::Cancelled),
        ] {
            let fixture = fixture_with_terminal(kind).await;
            let resolver = EpisodeSourceResolver::new(
                &fixture.history,
                &fixture.digest,
                EpisodeSourceLimits::repo_v1(),
            )
            .expect("construct source resolver");
            let source = resolver
                .resolve(&fixture.context, &fixture.target, fixture.source_head)
                .await
                .expect("resolve terminal task source");
            assert_eq!(source.facts().completion_status, expected);
        }
    }

    #[tokio::test]
    async fn admission_maps_only_resolver_evidence_and_preserves_low_confidence() {
        let fixture = fixture().await;
        let resolver = EpisodeSourceResolver::new(
            &fixture.history,
            &fixture.digest,
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct source resolver");
        let source = resolver
            .resolve(&fixture.context, &fixture.target, fixture.source_head)
            .await
            .expect("resolve task source");
        let fragment_id = source.fragments()[0].fragment_id().to_string();
        let compiler = FakeCompiler {
            evidence_fragment_id: fragment_id,
        };
        let config = EpisodeCompileConfig::new(
            "libra-memory/1",
            1,
            "task-episode-v1",
            "deterministic-test-provider",
        )
        .expect("construct compiler config");
        let admitted = EpisodeAdmission::new(&fixture.digest)
            .compile(
                &compiler,
                &config,
                &fixture.context,
                &fixture.target,
                source,
            )
            .await
            .expect("admit deterministic compiler proposal");
        let note = admitted.proposal().note();
        let episode = note.episode.as_ref().expect("Episode payload must exist");
        assert_eq!(episode.completion_status, CompletionStatus::Completed);
        for forbidden in [SECRET, "alice@example.com", "/Users/alice", "hidden note"] {
            assert!(!episode.goal.claim.contains(forbidden));
        }
        assert!(episode.goal.claim.contains("<REDACTED:email>"));
        assert_eq!(
            episode.inferences[0].confidence,
            Some(MemoryAnchorConfidence::Low)
        );
        assert_eq!(note.compile_record.producer, "libra-memory/1");
        assert!(
            note.compile_record
                .idempotency_key
                .starts_with("hmac-sha256:")
        );
        assert!(
            note.evidence_refs
                .iter()
                .all(|evidence| evidence.source_ref_oid == fixture.source_head.to_string())
        );
    }

    #[tokio::test]
    async fn admission_rejects_compiler_invented_evidence_fragment() {
        let fixture = fixture().await;
        let resolver = EpisodeSourceResolver::new(
            &fixture.history,
            &fixture.digest,
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct source resolver");
        let source = resolver
            .resolve(&fixture.context, &fixture.target, fixture.source_head)
            .await
            .expect("resolve task source");
        let config = EpisodeCompileConfig::new(
            "libra-memory/1",
            1,
            "task-episode-v1",
            "deterministic-test-provider",
        )
        .expect("construct compiler config");
        let Err(error) = EpisodeAdmission::new(&fixture.digest)
            .compile(
                &FakeCompiler {
                    evidence_fragment_id: "task:invented".to_string(),
                },
                &config,
                &fixture.context,
                &fixture.target,
                source,
            )
            .await
        else {
            panic!("invented evidence fragment must be rejected");
        };
        assert_eq!(error.kind(), EpisodeAdmissionErrorKind::InvalidProposal);
    }

    #[tokio::test]
    async fn writer_commits_only_after_exact_source_revalidation() {
        let fixture = fixture().await;
        let resolver = EpisodeSourceResolver::new(
            &fixture.history,
            &fixture.digest,
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct source resolver");
        let source = resolver
            .resolve(&fixture.context, &fixture.target, fixture.source_head)
            .await
            .expect("resolve task source");
        let compiler = FakeCompiler {
            evidence_fragment_id: source.fragments()[0].fragment_id().to_string(),
        };
        let config = EpisodeCompileConfig::new(
            "libra-memory/1",
            1,
            "task-episode-v1",
            "deterministic-test-provider",
        )
        .expect("construct compiler config");
        let admitted = EpisodeAdmission::new(&fixture.digest)
            .compile(
                &compiler,
                &config,
                &fixture.context,
                &fixture.target,
                source,
            )
            .await
            .expect("admit compiler proposal");
        let writer = MemoryWriter::for_tests(
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
            Arc::clone(&fixture.digest),
        )
        .await
        .expect("construct Memory writer");
        let committed = writer
            .commit_admitted(
                &resolver,
                &fixture.context,
                &fixture.target,
                &admitted,
                None,
                None,
            )
            .await
            .expect("commit revalidated admitted Episode");
        assert!(committed.appended());
        assert_eq!(committed.note_id(), fixture.target.root().note_id());
        assert_eq!(committed.event_seq(), 2);
    }

    #[test]
    fn object_type_constants_match_persisted_git_internal_names() {
        assert_eq!(ObjectType::Task.to_string(), TASK);
        assert_eq!(ObjectType::Intent.to_string(), INTENT);
        assert_eq!(ObjectType::Run.to_string(), RUN);
        assert_eq!(ObjectType::ContextFrame.to_string(), CONTEXT_FRAME);
    }
}
