use std::collections::BTreeSet;

use git_internal::hash::ObjectHash;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    domain::{
        EvidenceKind, EvidenceLocatorV1, EvidenceRefV1, EvidenceSourcePlane, MemoryNoteV1,
        ToolCallPart,
    },
    policy::{AuthenticatedMemoryContext, REPO_EPISODE_POLICY_VERSION, authorizes_evidence_read},
    replay::{ReducedProjection, ReplayRecord},
    source::{EpisodeSourceError, MemorySourceRedactor, compact_task_episode_bytes},
    tree::load_history_delta_bounded,
};
use crate::internal::ai::history::HistoryManager;

const MAX_EVIDENCE_ITEMS: usize = 64;
const MAX_EVIDENCE_TOTAL_BYTES: usize = 512 * 1024;
const MAX_EVIDENCE_OBJECT_BYTES: u64 = 512 * 1024;
const MAX_EVIDENCE_TREE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EVIDENCE_ANCESTRY: usize = 2_048;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedEvidenceV1 {
    pub(crate) reference: EvidenceRefV1,
    pub(crate) redacted_text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EvidenceOmissionReason {
    LimitExceeded,
    Unauthorized,
    SourceUnreachable,
    SourceCorrupt,
    DigestMismatch,
    UnsupportedLocator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EvidenceOmissionV1 {
    pub(crate) object_id: String,
    pub(crate) reason: EvidenceOmissionReason,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EvidenceExpansionV1 {
    pub(crate) resolved: Vec<ResolvedEvidenceV1>,
    pub(crate) omissions: Vec<EvidenceOmissionV1>,
}

pub(crate) struct EvidenceResolver<'a> {
    history: &'a HistoryManager,
    redactor: MemorySourceRedactor,
}

impl<'a> EvidenceResolver<'a> {
    pub(crate) fn new(history: &'a HistoryManager) -> Result<Self, EpisodeSourceError> {
        Ok(Self {
            history,
            redactor: MemorySourceRedactor::new()?,
        })
    }

    pub(crate) async fn expand(
        &self,
        context: &AuthenticatedMemoryContext,
        view_repository_id: &str,
        note: &MemoryNoteV1,
        frozen_memory_head: Option<ObjectHash>,
    ) -> EvidenceExpansionV1 {
        let mut expansion = EvidenceExpansionV1::default();
        let mut seen = BTreeSet::new();
        let mut total_bytes = 0usize;
        for evidence in evidence_refs(note) {
            let key = match serde_json::to_string(evidence) {
                Ok(key) => key,
                Err(_) => {
                    expansion.omissions.push(EvidenceOmissionV1 {
                        object_id: evidence.object_id.clone(),
                        reason: EvidenceOmissionReason::SourceCorrupt,
                    });
                    continue;
                }
            };
            if !seen.insert(key) {
                continue;
            }
            if expansion.resolved.len() + expansion.omissions.len() == MAX_EVIDENCE_ITEMS {
                expansion.omissions.push(EvidenceOmissionV1 {
                    object_id: evidence.object_id.clone(),
                    reason: EvidenceOmissionReason::LimitExceeded,
                });
                break;
            }
            match self
                .resolve_one(
                    context,
                    view_repository_id,
                    note,
                    evidence,
                    frozen_memory_head,
                )
                .await
            {
                Ok(text) if total_bytes.saturating_add(text.len()) <= MAX_EVIDENCE_TOTAL_BYTES => {
                    total_bytes = total_bytes.saturating_add(text.len());
                    expansion.resolved.push(ResolvedEvidenceV1 {
                        reference: evidence.clone(),
                        redacted_text: text,
                    });
                }
                Ok(_) => expansion.omissions.push(EvidenceOmissionV1 {
                    object_id: evidence.object_id.clone(),
                    reason: EvidenceOmissionReason::LimitExceeded,
                }),
                Err(reason) => expansion.omissions.push(EvidenceOmissionV1 {
                    object_id: evidence.object_id.clone(),
                    reason,
                }),
            }
        }
        expansion
    }

    async fn resolve_one(
        &self,
        context: &AuthenticatedMemoryContext,
        view_repository_id: &str,
        note: &MemoryNoteV1,
        evidence: &EvidenceRefV1,
        frozen_memory_head: Option<ObjectHash>,
    ) -> Result<String, EvidenceOmissionReason> {
        if !authorizes_evidence_read(context, view_repository_id, note, evidence) {
            return Err(EvidenceOmissionReason::Unauthorized);
        }
        if !matches!(
            evidence.source_plane,
            EvidenceSourcePlane::AgentRuntime | EvidenceSourcePlane::Session
        ) {
            return Err(EvidenceOmissionReason::UnsupportedLocator);
        }
        match &evidence.locator {
            EvidenceLocatorV1::Object => {}
            EvidenceLocatorV1::ToolCall {
                invocation_id,
                part: ToolCallPart::Invocation,
            } if invocation_id == &evidence.object_id => {}
            _ => return Err(EvidenceOmissionReason::UnsupportedLocator),
        }
        let source_oid = evidence
            .source_ref_oid
            .parse::<ObjectHash>()
            .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
        match self.resolve_ai_history(evidence, source_oid).await {
            Ok(text) => return Ok(text),
            Err(EvidenceOmissionReason::SourceUnreachable)
                if evidence.kind == EvidenceKind::Task
                    && matches!(evidence.locator, EvidenceLocatorV1::Object) => {}
            Err(reason) => return Err(reason),
        }
        let frozen_memory_head =
            frozen_memory_head.ok_or(EvidenceOmissionReason::SourceUnreachable)?;
        self.resolve_memory_task_episode(evidence, source_oid, frozen_memory_head)
            .await
    }

    async fn resolve_ai_history(
        &self,
        evidence: &EvidenceRefV1,
        source_oid: ObjectHash,
    ) -> Result<String, EvidenceOmissionReason> {
        let view = self
            .history
            .pin_history(source_oid, MAX_EVIDENCE_ANCESTRY, MAX_EVIDENCE_TREE_BYTES)
            .await
            .map_err(|_| EvidenceOmissionReason::SourceUnreachable)?;
        let mut matched = None;
        for object_type in object_types(evidence.kind) {
            let blob = view
                .get_blob(object_type, &evidence.object_id, MAX_EVIDENCE_OBJECT_BYTES)
                .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
            let Some(blob) = blob else {
                continue;
            };
            let value: Value = serde_json::from_slice(blob.bytes())
                .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
            if value.get("object_id").and_then(Value::as_str) != Some(&evidence.object_id)
                || value.get("object_type").and_then(Value::as_str) != Some(object_type)
            {
                return Err(EvidenceOmissionReason::SourceCorrupt);
            }
            let redacted = self
                .redactor
                .redact(blob.bytes())
                .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
            let digest = format!("sha256:{}", hex::encode(Sha256::digest(&redacted)));
            if digest == evidence.fragment_digest {
                let text = String::from_utf8(redacted)
                    .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
                if matched.replace(text).is_some() {
                    return Err(EvidenceOmissionReason::SourceCorrupt);
                }
            }
        }
        matched.ok_or(EvidenceOmissionReason::DigestMismatch)
    }

    async fn resolve_memory_task_episode(
        &self,
        evidence: &EvidenceRefV1,
        source_oid: ObjectHash,
        frozen_memory_head: ObjectHash,
    ) -> Result<String, EvidenceOmissionReason> {
        load_history_delta_bounded(
            self.history.repository_path(),
            frozen_memory_head,
            Some(source_oid),
            REPO_EPISODE_POLICY_VERSION,
            MAX_EVIDENCE_ANCESTRY,
        )
        .map_err(|_| EvidenceOmissionReason::SourceUnreachable)?;
        let history = load_history_delta_bounded(
            self.history.repository_path(),
            source_oid,
            None,
            REPO_EPISODE_POLICY_VERSION,
            MAX_EVIDENCE_ANCESTRY,
        )
        .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
        let mut reduced = ReducedProjection::default();
        for record in history.records {
            reduced
                .apply(ReplayRecord {
                    event: record.event,
                    revision_oid: record.revision_oid,
                    note: record.note,
                })
                .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
        }
        let note_id = evidence
            .object_id
            .parse::<uuid::Uuid>()
            .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
        let projected = reduced
            .notes
            .get(&note_id)
            .ok_or(EvidenceOmissionReason::SourceCorrupt)?;
        let revision_oid = projected
            .live_revision_oid
            .ok_or(EvidenceOmissionReason::SourceCorrupt)?;
        let note = reduced
            .new_revisions
            .get(&revision_oid.to_string())
            .ok_or(EvidenceOmissionReason::SourceCorrupt)?;
        let task_id = note
            .episode
            .as_ref()
            .map(|episode| episode.root_id.as_str())
            .ok_or(EvidenceOmissionReason::SourceCorrupt)?;
        let raw = compact_task_episode_bytes(task_id, note)
            .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
        let redacted = self
            .redactor
            .redact(&raw)
            .map_err(|_| EvidenceOmissionReason::SourceCorrupt)?;
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(&redacted)));
        if digest != evidence.fragment_digest {
            return Err(EvidenceOmissionReason::DigestMismatch);
        }
        String::from_utf8(redacted).map_err(|_| EvidenceOmissionReason::SourceCorrupt)
    }
}

fn evidence_refs(note: &MemoryNoteV1) -> Vec<&EvidenceRefV1> {
    let mut refs = Vec::new();
    refs.extend(note.evidence_refs.iter());
    for link in &note.links {
        refs.extend(link.evidence_refs.iter());
    }
    for entity in &note.entities {
        refs.extend(entity.evidence_refs.iter());
    }
    if let Some(episode) = &note.episode {
        refs.extend(episode.goal.evidence_refs.iter());
        refs.extend(episode.summary.evidence_refs.iter());
        for claim in episode
            .observations
            .iter()
            .chain(&episode.inferences)
            .chain(&episode.decisions)
            .chain(&episode.failed_attempts)
            .chain(&episode.unresolved)
        {
            refs.extend(claim.evidence_refs.iter());
        }
    }
    refs
}

fn object_types(kind: EvidenceKind) -> &'static [&'static str] {
    match kind {
        EvidenceKind::Intent => &["intent", "intent_event"],
        EvidenceKind::Task => &["task", "task_event"],
        EvidenceKind::Run => &["run", "run_event"],
        EvidenceKind::Evidence => &["evidence", "context_frame"],
        EvidenceKind::Decision => &["decision"],
        EvidenceKind::PatchSet => &["patchset"],
        EvidenceKind::Session => &["session"],
        EvidenceKind::ToolCall => &["invocation"],
        EvidenceKind::Code => &[],
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        internal::ai::memory::{
            domain::EvidenceVisibility,
            writer::tests::{fixture, proposal},
        },
        utils::{object::write_git_object, storage::local::LocalStorage},
    };

    fn digest(bytes: &[u8]) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
    }

    fn clear_evidence(note: &mut MemoryNoteV1) {
        note.evidence_refs.clear();
        note.links.clear();
        note.entities.clear();
        let episode = note.episode.as_mut().expect("Episode payload");
        episode.goal.evidence_refs.clear();
        episode.summary.evidence_refs.clear();
        for claim in episode
            .observations
            .iter_mut()
            .chain(&mut episode.inferences)
            .chain(&mut episode.decisions)
            .chain(&mut episode.failed_attempts)
            .chain(&mut episode.unresolved)
        {
            claim.evidence_refs.clear();
        }
    }

    #[tokio::test]
    async fn reader_evidence_authz_and_memory_episode_expansion() {
        let fixture = fixture().await;
        let storage = Arc::new(LocalStorage::new(fixture._temp.path().join("objects")));
        let history = HistoryManager::new(
            storage,
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        );

        let runtime_bytes = serde_json::to_vec(&serde_json::json!({
            "object_id": "task-runtime-evidence",
            "object_type": "task",
            "summary": "the retry succeeded after the bounded backoff"
        }))
        .expect("serialize runtime evidence");
        let redactor = MemorySourceRedactor::new().expect("construct redactor");
        let runtime_redacted = redactor
            .redact(&runtime_bytes)
            .expect("redact runtime evidence");
        let blob_oid = write_git_object(fixture._temp.path(), "blob", &runtime_bytes)
            .expect("write runtime evidence blob");
        history
            .append("task", "task-runtime-evidence", blob_oid)
            .await
            .expect("append runtime evidence");
        let runtime_head = history
            .resolve_history_head()
            .await
            .expect("read runtime head")
            .expect("runtime head exists");
        let runtime_ref = EvidenceRefV1 {
            schema_version: 1,
            source_plane: EvidenceSourcePlane::AgentRuntime,
            kind: EvidenceKind::Task,
            object_id: "task-runtime-evidence".to_string(),
            source_ref_oid: runtime_head.to_string(),
            locator: EvidenceLocatorV1::Object,
            fragment_digest: digest(&runtime_redacted),
            visibility: EvidenceVisibility::RepoLocal,
            captured_at: None,
            code_commit: None,
        };

        let task_proposal = proposal(&fixture.target, fixture.key_id, 1);
        let committed = fixture
            .writer
            .commit(&fixture.context, &fixture.target, &task_proposal, None)
            .await
            .expect("commit source Task Episode");
        let memory_bytes =
            compact_task_episode_bytes(fixture.target.root().id(), task_proposal.note())
                .expect("serialize compact Task Episode");
        let memory_redacted = redactor
            .redact(&memory_bytes)
            .expect("redact compact Task Episode");
        let memory_ref = EvidenceRefV1 {
            schema_version: 1,
            source_plane: EvidenceSourcePlane::AgentRuntime,
            kind: EvidenceKind::Task,
            object_id: fixture.target.root().note_id().to_string(),
            source_ref_oid: committed.commit_oid().to_string(),
            locator: EvidenceLocatorV1::Object,
            fragment_digest: digest(&memory_redacted),
            visibility: EvidenceVisibility::RepoLocal,
            captured_at: None,
            code_commit: None,
        };
        let mut private_ref = runtime_ref.clone();
        private_ref.visibility = EvidenceVisibility::Private;

        let mut carrier = proposal(&fixture.target, fixture.key_id, 2);
        clear_evidence(carrier.note_mut());
        carrier.note_mut().evidence_refs = vec![runtime_ref, memory_ref, private_ref];
        let expansion = EvidenceResolver::new(&history)
            .expect("construct resolver")
            .expand(
                &fixture.context,
                fixture.context.repository_id(),
                carrier.note(),
                Some(committed.commit_oid()),
            )
            .await;
        assert_eq!(expansion.resolved.len(), 2);
        assert_eq!(
            expansion
                .resolved
                .iter()
                .map(|item| item.reference.object_id.clone())
                .collect::<Vec<_>>(),
            [
                "task-runtime-evidence".to_string(),
                fixture.target.root().note_id().to_string(),
            ],
        );
        assert_eq!(
            expansion.omissions,
            [EvidenceOmissionV1 {
                object_id: "task-runtime-evidence".to_string(),
                reason: EvidenceOmissionReason::Unauthorized,
            }],
        );

        let foreign_context =
            AuthenticatedMemoryContext::new("foreign-repository", fixture.context.actor().clone())
                .expect("construct foreign repository context");
        let foreign = EvidenceResolver::new(&history)
            .expect("construct foreign resolver")
            .expand(
                &foreign_context,
                fixture.context.repository_id(),
                carrier.note(),
                Some(committed.commit_oid()),
            )
            .await;
        assert!(foreign.resolved.is_empty());
        assert_eq!(foreign.omissions.len(), 3);
        assert!(
            foreign
                .omissions
                .iter()
                .all(|item| item.reason == EvidenceOmissionReason::Unauthorized)
        );
    }
}
