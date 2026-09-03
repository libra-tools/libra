use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    domain::{
        ActorKind, ActorRefV1, EpisodeClaimV1, EpisodeRoot, EvidenceRefV1, EvidenceVisibility,
        MemoryNoteV1, MemoryScopeV1, MemorySensitivity, MemoryTrust, MemoryVisibility,
    },
    error::{MemoryWriterError, MemoryWriterErrorKind},
};

pub(super) const REPO_EPISODE_POLICY_VERSION: &str = "repo-policy-v1";
pub(super) const REPO_EPISODE_ACL_POLICY_ID: &str = "repo-default-v1";
pub(super) const REPO_EPISODE_PRODUCER: &str = "libra-memory/1";
const REPO_EPISODE_POLICY_SNAPSHOT: &[u8] = br#"{
  "acl_policy":"repo-default-v1",
  "auto_confirm":true,
  "producer":"libra-memory/1",
  "scope":"repo",
  "transport":"local_only",
  "visibility":"repo_local",
  "writer_version":1
}"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedMemoryContext {
    repository_id: String,
    actor: ActorRefV1,
}

/// Apply the fixed repository-local read policy to one immutable note.
///
/// M2 has no user-maintained ACL service: any authenticated principal running
/// inside the owning repository may read repo-local, non-secret Episode
/// material. Keeping this check here still makes every read depend on the
/// authenticated repository identity and the persisted policy labels.
pub(super) fn authorizes_note_read(
    context: &AuthenticatedMemoryContext,
    view_repository_id: &str,
    note: &MemoryNoteV1,
) -> bool {
    context.repository_id() == view_repository_id
        && note.namespace == "default"
        && note.scope == MemoryScopeV1::Repo
        && note.visibility == MemoryVisibility::RepoLocal
        && note.acl_policy_id == REPO_EPISODE_ACL_POLICY_ID
        && note.compile_record.policy_version == REPO_EPISODE_POLICY_VERSION
        && note.trust == MemoryTrust::RepoEvidence
        && note.sensitivity != MemorySensitivity::SecretLike
}

/// Authorize one evidence reference independently before resolving its source.
pub(super) fn authorizes_evidence_read(
    context: &AuthenticatedMemoryContext,
    view_repository_id: &str,
    note: &MemoryNoteV1,
    evidence: &EvidenceRefV1,
) -> bool {
    authorizes_note_read(context, view_repository_id, note)
        && evidence.visibility == EvidenceVisibility::RepoLocal
}

impl AuthenticatedMemoryContext {
    pub(crate) fn new(
        repository_id: impl Into<String>,
        actor: ActorRefV1,
    ) -> Result<Self, MemoryWriterError> {
        let repository_id = repository_id.into();
        if repository_id.is_empty() || actor.principal_id.is_empty() {
            return Err(MemoryWriterError::new(
                MemoryWriterErrorKind::PolicyRejected,
                "authenticated repository and actor identity are required",
            ));
        }
        Ok(Self {
            repository_id,
            actor,
        })
    }

    pub(crate) fn repository_id(&self) -> &str {
        &self.repository_id
    }

    pub(crate) fn actor(&self) -> &ActorRefV1 {
        &self.actor
    }

    /// Build the repository-local system principal used by trusted Libra
    /// maintenance and diagnostic adapters.
    pub(crate) fn repository_system(
        repository_id: impl Into<String>,
        principal_id: impl Into<String>,
    ) -> Result<Self, MemoryWriterError> {
        Self::new(
            repository_id,
            ActorRefV1 {
                kind: ActorKind::System,
                principal_id: principal_id.into(),
            },
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrustedMemoryTarget {
    root: EpisodeRoot,
}

impl TrustedMemoryTarget {
    pub(crate) fn episode(root: EpisodeRoot) -> Self {
        Self { root }
    }

    pub(crate) fn root(&self) -> &EpisodeRoot {
        &self.root
    }

    pub(crate) const fn scope_key(&self) -> &'static str {
        "repo"
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeterministicMemoryProposal {
    note: MemoryNoteV1,
}

impl DeterministicMemoryProposal {
    pub(crate) fn new(note: MemoryNoteV1) -> Self {
        Self { note }
    }

    pub(crate) fn note(&self) -> &MemoryNoteV1 {
        &self.note
    }

    #[cfg(test)]
    pub(super) fn note_mut(&mut self) -> &mut MemoryNoteV1 {
        &mut self.note
    }
}

pub(super) fn validate_writer_policy(
    context: &AuthenticatedMemoryContext,
    target: &TrustedMemoryTarget,
    proposal: &DeterministicMemoryProposal,
    repository_id: &str,
    key_id: Uuid,
) -> Result<(), MemoryWriterError> {
    let note = proposal.note();
    if context.repository_id() != repository_id {
        return Err(policy_error(
            "authenticated repository does not match writer repository",
        ));
    }
    if note.compile_record.policy_version != REPO_EPISODE_POLICY_VERSION {
        return Err(policy_error(
            "proposal policy version is not supported by the repo Episode writer",
        ));
    }
    if note.acl_policy_id != REPO_EPISODE_ACL_POLICY_ID {
        return Err(policy_error(
            "proposal ACL policy is not supported by the repo Episode writer",
        ));
    }
    if note.sensitivity == MemorySensitivity::SecretLike {
        return Err(policy_error(
            "secret-like Memory cannot enter the unencrypted repository object store",
        ));
    }
    if note.compile_record.origin != super::domain::CompileOriginV1::EpisodeCompiler
        || note.compile_record.producer != REPO_EPISODE_PRODUCER
    {
        return Err(policy_error(
            "proposal producer is not supported by the repo Episode writer",
        ));
    }
    if &note.author != context.actor() {
        return Err(policy_error(
            "proposal author does not match authenticated actor",
        ));
    }
    if note.note_id != target.root().note_id()
        || note.namespace != target.root().namespace()
        || note.path != target.root().path()
        || note.scope != MemoryScopeV1::Repo
        || note.visibility != MemoryVisibility::RepoLocal
        || note.episode.as_ref().is_none_or(|episode| {
            episode.root_kind != target.root().kind() || episode.root_id != target.root().id()
        })
    {
        return Err(policy_error(
            "proposal does not match the trusted Episode target",
        ));
    }

    validate_digest_key(&note.compile_record.idempotency_key, key_id)?;
    for digest in &note.compile_record.input_hashes {
        validate_optional_digest_key(digest, key_id)?;
    }
    for evidence in all_evidence(note) {
        validate_optional_digest_key(&evidence.fragment_digest, key_id)?;
    }
    Ok(())
}

pub(super) fn policy_snapshot_digest(policy_version: &str) -> Result<String, MemoryWriterError> {
    if policy_version != REPO_EPISODE_POLICY_VERSION {
        return Err(policy_error(
            "Memory history uses an unsupported repo Episode policy version",
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(policy_version.as_bytes());
    hasher.update(b"\0");
    hasher.update(REPO_EPISODE_POLICY_SNAPSHOT);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn all_evidence(note: &MemoryNoteV1) -> Vec<&EvidenceRefV1> {
    let mut evidence = Vec::new();
    evidence.extend(note.evidence_refs.iter());
    for link in &note.links {
        evidence.extend(link.evidence_refs.iter());
    }
    for entity in &note.entities {
        evidence.extend(entity.evidence_refs.iter());
    }
    if let Some(episode) = &note.episode {
        push_claim_evidence(&mut evidence, &episode.goal);
        push_claim_evidence(&mut evidence, &episode.summary);
        for claim in episode
            .observations
            .iter()
            .chain(&episode.inferences)
            .chain(&episode.decisions)
            .chain(&episode.failed_attempts)
            .chain(&episode.unresolved)
        {
            push_claim_evidence(&mut evidence, claim);
        }
    }
    evidence
}

fn push_claim_evidence<'a>(output: &mut Vec<&'a EvidenceRefV1>, claim: &'a EpisodeClaimV1) {
    output.extend(claim.evidence_refs.iter());
}

fn validate_optional_digest_key(digest: &str, expected: Uuid) -> Result<(), MemoryWriterError> {
    if digest.starts_with("hmac-sha256:") {
        validate_digest_key(digest, expected)?;
    }
    Ok(())
}

fn validate_digest_key(digest: &str, expected: Uuid) -> Result<(), MemoryWriterError> {
    let mut parts = digest.split(':');
    let algorithm = parts.next();
    let key = parts.next();
    let value = parts.next();
    if algorithm != Some("hmac-sha256") || value.is_none() || parts.next().is_some() {
        return Err(policy_error("keyed digest envelope is malformed"));
    }
    let key_id = key
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| policy_error("keyed digest key ID is malformed"))?;
    if key_id != expected {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::UnknownDigestKey,
            "proposal references an unknown repository digest key",
        ));
    }
    Ok(())
}

fn policy_error(summary: &'static str) -> MemoryWriterError {
    MemoryWriterError::new(MemoryWriterErrorKind::PolicyRejected, summary)
}
