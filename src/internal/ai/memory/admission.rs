use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;

use super::{
    compiler::{
        EpisodeClaimProposalV1, EpisodeCompileConfig, EpisodeCompiler, EpisodeCompilerErrorKind,
        EpisodeCompilerProposalV1,
        schema::{validate_claim as validate_claim_schema, validate_proposal},
    },
    domain::{
        CompileOriginV1, CompileRecordV1, EpisodeClaimV1, EpisodeOmissionsV1, EpisodePayloadV1,
        EpistemicStatus, IdempotencyScopeV1, MemoryKind, MemoryLifecycle, MemoryLinkKind,
        MemoryLinkV1, MemoryNoteV1, MemoryScopeV1, MemorySensitivity, MemoryTrust,
        MemoryVisibility,
    },
    policy::{
        AuthenticatedMemoryContext, DeterministicMemoryProposal, REPO_EPISODE_ACL_POLICY_ID,
        REPO_EPISODE_POLICY_VERSION, REPO_EPISODE_PRODUCER, TrustedMemoryTarget,
    },
    source::RedactedEpisodeSource,
};
use crate::internal::ai::{
    context_budget::MemoryAnchorConfidence, keyed_digest::RepositoryKeyedDigest,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeAdmissionErrorKind {
    CompilerTransient,
    CompilerStable,
    InvalidProposal,
    SourceMismatch,
    DigestUnavailable,
}

#[derive(Debug, Error)]
#[error("Episode admission failed ({kind:?})")]
pub(crate) struct EpisodeAdmissionError {
    kind: EpisodeAdmissionErrorKind,
}

impl EpisodeAdmissionError {
    const fn new(kind: EpisodeAdmissionErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> EpisodeAdmissionErrorKind {
        self.kind
    }
}

/// A proposal that crossed the compiler seam and deterministic admission.
/// Only this module can construct it, and it retains the exact redacted source
/// so the writer can re-resolve every EvidenceRef immediately before commit.
pub(crate) struct AdmittedEpisodeProposal {
    proposal: DeterministicMemoryProposal,
    source: RedactedEpisodeSource,
}

impl AdmittedEpisodeProposal {
    pub(super) fn proposal(&self) -> &DeterministicMemoryProposal {
        &self.proposal
    }

    pub(super) fn source(&self) -> &RedactedEpisodeSource {
        &self.source
    }
}

pub(crate) struct EpisodeAdmission<'a> {
    digest: &'a RepositoryKeyedDigest,
}

impl<'a> EpisodeAdmission<'a> {
    pub(crate) const fn new(digest: &'a RepositoryKeyedDigest) -> Self {
        Self { digest }
    }

    pub(crate) async fn compile<C: EpisodeCompiler + ?Sized>(
        &self,
        compiler: &C,
        config: &EpisodeCompileConfig,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        source: RedactedEpisodeSource,
    ) -> Result<AdmittedEpisodeProposal, EpisodeAdmissionError> {
        if context.repository_id() != self.digest.repository_id()
            || source.manifest().root_kind != target.root().kind()
            || source.manifest().root_id != target.root().id()
            || config.producer() != REPO_EPISODE_PRODUCER
        {
            return Err(EpisodeAdmissionError::new(
                EpisodeAdmissionErrorKind::SourceMismatch,
            ));
        }
        let compiler_proposal =
            compiler
                .compile(&source, config)
                .await
                .map_err(|error| match error.kind() {
                    EpisodeCompilerErrorKind::ProviderFailed
                    | EpisodeCompilerErrorKind::ProviderTimedOut => {
                        EpisodeAdmissionError::new(EpisodeAdmissionErrorKind::CompilerTransient)
                    }
                    EpisodeCompilerErrorKind::InvalidConfig
                    | EpisodeCompilerErrorKind::MalformedOutput
                    | EpisodeCompilerErrorKind::OutputLimitExceeded
                    | EpisodeCompilerErrorKind::SensitiveOutput => {
                        EpisodeAdmissionError::new(EpisodeAdmissionErrorKind::CompilerStable)
                    }
                })?;
        let proposal = self.admit(config, context, target, &source, compiler_proposal)?;
        Ok(AdmittedEpisodeProposal { proposal, source })
    }

    fn admit(
        &self,
        config: &EpisodeCompileConfig,
        context: &AuthenticatedMemoryContext,
        target: &TrustedMemoryTarget,
        source: &RedactedEpisodeSource,
        proposal: EpisodeCompilerProposalV1,
    ) -> Result<DeterministicMemoryProposal, EpisodeAdmissionError> {
        validate_proposal(&proposal).map_err(|_| invalid_proposal())?;
        let idempotency_input = serde_json::to_vec(&IdempotencyInput {
            manifest: source.manifest(),
            config,
            proposal: &proposal,
        })
        .map_err(|_| invalid_proposal())?;
        let summary = claim(source, proposal.summary, Some(EpistemicStatus::Inference))?;
        let observations = claims(
            source,
            proposal.observations,
            Some(EpistemicStatus::Observation),
        )?;
        let inferences = claims(
            source,
            proposal.inferences,
            Some(EpistemicStatus::Inference),
        )?;
        let decisions = claims(source, proposal.decisions, None)?;
        let failed_attempts = claims(source, proposal.failed_attempts, None)?;
        let unresolved = claims(source, proposal.unresolved, None)?;
        let root_fragment = source.fragments().first().ok_or_else(invalid_proposal)?;
        let goal = EpisodeClaimV1 {
            epistemic_status: EpistemicStatus::Observation,
            claim: source.facts().root_goal.clone(),
            confidence: None,
            evidence_refs: vec![root_fragment.evidence().clone()],
        };
        let manifest_bytes =
            serde_json::to_vec(source.manifest()).map_err(|_| invalid_proposal())?;
        let manifest_fingerprint = self
            .digest
            .source_input_fingerprint(&manifest_bytes)
            .map_err(|_| EpisodeAdmissionError::new(EpisodeAdmissionErrorKind::DigestUnavailable))?
            .encoded();
        let idempotency_key = self
            .digest
            .source_input_fingerprint(&idempotency_input)
            .map_err(|_| EpisodeAdmissionError::new(EpisodeAdmissionErrorKind::DigestUnavailable))?
            .encoded();
        let source_evidence = source
            .fragments()
            .iter()
            .map(|fragment| fragment.evidence().clone())
            .collect::<Vec<_>>();
        let links = source
            .pinned_task_episodes()
            .iter()
            .map(|task| {
                let evidence = source
                    .evidence(task.fragment_id())
                    .ok_or_else(invalid_proposal)?;
                Ok(MemoryLinkV1 {
                    kind: MemoryLinkKind::Supports,
                    target_note_id: task.note_id(),
                    target_revision_oid: Some(task.revision_oid().to_string()),
                    evidence_refs: vec![evidence.clone()],
                    valid_from: None,
                    valid_until: None,
                })
            })
            .collect::<Result<Vec<_>, EpisodeAdmissionError>>()?;
        let related_run_omissions = source
            .manifest()
            .omissions
            .iter()
            .filter(|omission| omission.object_type == "run")
            .map(|omission| omission.count)
            .sum::<usize>()
            .try_into()
            .unwrap_or(u32::MAX);
        let episode = EpisodePayloadV1 {
            schema_version: 1,
            root_kind: target.root().kind(),
            root_id: target.root().id().to_string(),
            related_intent_ids: source.facts().related_intent_ids.clone(),
            related_task_ids: source.facts().related_task_ids.clone(),
            related_run_ids: source.facts().related_run_ids.clone(),
            started_at: Some(source.facts().started_at),
            ended_at: Some(source.facts().ended_at),
            goal,
            completion_status: source.facts().completion_status,
            code_change_status: source.facts().code_change_status,
            summary: summary.clone(),
            observations,
            inferences,
            decisions,
            failed_attempts,
            unresolved,
            code: source.facts().code.clone(),
            omissions: EpisodeOmissionsV1 {
                related_run_ids: related_run_omissions,
                ..EpisodeOmissionsV1::default()
            },
        };
        let note = MemoryNoteV1 {
            schema_version: 1,
            note_id: target.root().note_id(),
            content_digest: String::new(),
            namespace: target.root().namespace().to_string(),
            path: target.root().path().to_string(),
            kind: MemoryKind::Episodic,
            scope: MemoryScopeV1::Repo,
            visibility: MemoryVisibility::RepoLocal,
            acl_policy_id: REPO_EPISODE_ACL_POLICY_ID.to_string(),
            lifecycle: MemoryLifecycle::Accretive,
            body: summary.claim,
            rationale: None,
            episode: Some(episode),
            evidence_refs: source_evidence,
            links,
            entities: Vec::new(),
            parents: Vec::new(),
            tags: vec!["episode".to_string()],
            confidence: MemoryAnchorConfidence::Medium,
            trust: MemoryTrust::RepoEvidence,
            sensitivity: MemorySensitivity::Internal,
            valid_from: None,
            valid_until: None,
            effective_from_commit: source
                .facts()
                .code
                .result_oid
                .clone()
                .or_else(|| source.facts().code.base_oid.clone()),
            effective_until_commit: None,
            expires_at: None,
            author: context.actor().clone(),
            created_at: source.facts().ended_at,
            compile_record: CompileRecordV1 {
                schema_version: 1,
                origin: CompileOriginV1::EpisodeCompiler,
                producer: REPO_EPISODE_PRODUCER.to_string(),
                rules_version: config.rules_version(),
                prompt_version: Some(config.prompt_version().to_string()),
                model_id: Some(config.model_id().to_string()),
                policy_version: REPO_EPISODE_POLICY_VERSION.to_string(),
                input_hashes: vec![manifest_fingerprint],
                idempotency_key,
                idempotency_scope: IdempotencyScopeV1::Cell,
            },
        };
        Ok(DeterministicMemoryProposal::new(note))
    }
}

#[derive(Serialize)]
struct IdempotencyInput<'a> {
    manifest: &'a super::source::EpisodeSourceManifestV1,
    config: &'a EpisodeCompileConfig,
    proposal: &'a EpisodeCompilerProposalV1,
}

fn claims(
    source: &RedactedEpisodeSource,
    proposals: Vec<EpisodeClaimProposalV1>,
    required_status: Option<EpistemicStatus>,
) -> Result<Vec<EpisodeClaimV1>, EpisodeAdmissionError> {
    proposals
        .into_iter()
        .map(|proposal| claim(source, proposal, required_status))
        .collect()
}

fn claim(
    source: &RedactedEpisodeSource,
    proposal: EpisodeClaimProposalV1,
    required_status: Option<EpistemicStatus>,
) -> Result<EpisodeClaimV1, EpisodeAdmissionError> {
    validate_claim_schema(&proposal, required_status).map_err(|_| invalid_proposal())?;
    let mut seen = BTreeSet::new();
    let mut evidence_refs = Vec::new();
    for fragment_id in proposal.evidence_fragment_ids {
        if !seen.insert(fragment_id.clone()) {
            continue;
        }
        let evidence = source.evidence(&fragment_id).ok_or_else(invalid_proposal)?;
        evidence_refs.push(evidence.clone());
    }
    Ok(EpisodeClaimV1 {
        epistemic_status: proposal.epistemic_status,
        claim: proposal.claim,
        confidence: proposal.confidence,
        evidence_refs,
    })
}

fn invalid_proposal() -> EpisodeAdmissionError {
    EpisodeAdmissionError::new(EpisodeAdmissionErrorKind::InvalidProposal)
}
