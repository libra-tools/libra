use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use super::super::domain::EpistemicStatus;
use crate::internal::ai::context_budget::MemoryAnchorConfidence;

pub(crate) const MAX_CLAIM_BYTES: usize = 4 * 1024;
pub(crate) const MAX_CLAIMS_PER_SECTION: usize = 64;
pub(crate) const MAX_COMPILER_OUTPUT_BYTES: usize = 16 * 1024;
pub(crate) const MAX_EVIDENCE_PER_CLAIM: usize = 32;
const MAX_FRAGMENT_ID_BYTES: usize = 512;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EpisodeClaimProposalV1 {
    pub(crate) epistemic_status: EpistemicStatus,
    pub(crate) claim: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(crate) confidence: Option<MemoryAnchorConfidence>,
    pub(crate) evidence_fragment_ids: Vec<String>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EpisodeCompilerProposalV1 {
    pub(crate) summary: EpisodeClaimProposalV1,
    pub(crate) observations: Vec<EpisodeClaimProposalV1>,
    pub(crate) inferences: Vec<EpisodeClaimProposalV1>,
    pub(crate) decisions: Vec<EpisodeClaimProposalV1>,
    pub(crate) failed_attempts: Vec<EpisodeClaimProposalV1>,
    pub(crate) unresolved: Vec<EpisodeClaimProposalV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProposalValidationErrorKind {
    Malformed,
    OutputLimitExceeded,
}

pub(crate) fn validate_proposal(
    proposal: &EpisodeCompilerProposalV1,
) -> Result<(), ProposalValidationErrorKind> {
    let encoded =
        serde_json::to_vec(proposal).map_err(|_| ProposalValidationErrorKind::Malformed)?;
    if encoded.len() > MAX_COMPILER_OUTPUT_BYTES {
        return Err(ProposalValidationErrorKind::OutputLimitExceeded);
    }

    validate_claim(&proposal.summary, Some(EpistemicStatus::Inference))?;
    validate_section(&proposal.observations, Some(EpistemicStatus::Observation))?;
    validate_section(&proposal.inferences, Some(EpistemicStatus::Inference))?;
    validate_section(&proposal.decisions, None)?;
    validate_section(&proposal.failed_attempts, None)?;
    validate_section(&proposal.unresolved, None)?;
    Ok(())
}

fn validate_section(
    claims: &[EpisodeClaimProposalV1],
    required_status: Option<EpistemicStatus>,
) -> Result<(), ProposalValidationErrorKind> {
    if claims.len() > MAX_CLAIMS_PER_SECTION {
        return Err(ProposalValidationErrorKind::OutputLimitExceeded);
    }
    for claim in claims {
        validate_claim(claim, required_status)?;
    }
    Ok(())
}

pub(crate) fn validate_claim(
    proposal: &EpisodeClaimProposalV1,
    required_status: Option<EpistemicStatus>,
) -> Result<(), ProposalValidationErrorKind> {
    if proposal.claim.trim().is_empty()
        || proposal.claim.len() > MAX_CLAIM_BYTES
        || proposal.evidence_fragment_ids.is_empty()
        || proposal.evidence_fragment_ids.len() > MAX_EVIDENCE_PER_CLAIM
        || required_status.is_some_and(|status| status != proposal.epistemic_status)
        || match proposal.epistemic_status {
            EpistemicStatus::Observation => proposal.confidence.is_some(),
            EpistemicStatus::Inference => proposal.confidence.is_none(),
        }
    {
        return Err(ProposalValidationErrorKind::Malformed);
    }

    let mut evidence_ids = BTreeSet::new();
    for fragment_id in &proposal.evidence_fragment_ids {
        if fragment_id.trim().is_empty()
            || fragment_id.len() > MAX_FRAGMENT_ID_BYTES
            || !evidence_ids.insert(fragment_id)
        {
            return Err(ProposalValidationErrorKind::Malformed);
        }
    }
    Ok(())
}
