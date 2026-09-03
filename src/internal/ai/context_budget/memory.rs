//! Audited, budget-owned projection of Episode Memory into one prompt request.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    ContextBudget, ContextBudgetAllocator, ContextBudgetCandidate, ContextSegmentKind,
    allocator::AllocationOmissionReason,
    receipt::{
        ContextSelectionReceiptDraftV1, ContextSelectionReceiptV1, ReceiptDraftFieldsV1,
        ReceiptOmissionV1, ReceiptReproducibilityState, ReceiptSelectionInputV1,
        ReceiptSensitivity, ReceiptSourceKind,
    },
    receipt_store::ReceiptStore,
};
use crate::internal::ai::{
    history::HistoryManager,
    keyed_digest::RepositoryKeyedDigest,
    memory::{
        AuthenticatedMemoryContext, CodeApplicability, CompletionStatus, EpisodeQueryV1,
        EpisodeReadItemV1, EpisodeReader, EpisodeReaderError, EpisodeReaderErrorKind,
        EpisodeRootKind, EvidenceKind, EvidenceRefV1, EvidenceSourcePlane, MAX_CANDIDATES,
        MAX_RESULT_LIMIT, MemoryNoteV1, MemoryScopeV1, MemorySensitivity, MemoryTrust,
        ResolvedMemoryViewV1,
    },
};

const MEMORY_CONTEXT_SELECTOR_VERSION: &str = "episode-fts-bm25-v1+context-budget-v1";
const MEMORY_CONTEXT_SCHEMA_VERSION: u32 = 1;
const MEMORY_HEADER_ID: &str = "libra-memory-context-header-v1";

/// Prompt material that can only be constructed after its selection receipt
/// has been durably appended. It is transient and never becomes a persisted
/// `MemoryAnchor` or a second copy of the source `MemoryNote`.
pub(crate) struct AuditedMemoryContextBundleV1 {
    view_hash: String,
    prompt_section: String,
    receipt: ContextSelectionReceiptV1,
}

impl AuditedMemoryContextBundleV1 {
    pub(crate) fn view_hash(&self) -> &str {
        &self.view_hash
    }

    pub(crate) fn prompt_section(&self) -> &str {
        &self.prompt_section
    }

    pub(crate) fn receipt(&self) -> &ContextSelectionReceiptV1 {
        &self.receipt
    }
}

/// Shared context-layer owner for Memory selection, budget allocation,
/// receipt persistence, and prompt-bundle delivery.
pub(crate) struct MemoryContextAssembler<'a> {
    history: &'a HistoryManager,
    digest: Arc<RepositoryKeyedDigest>,
}

impl<'a> MemoryContextAssembler<'a> {
    pub(crate) fn new(history: &'a HistoryManager, digest: Arc<RepositoryKeyedDigest>) -> Self {
        Self { history, digest }
    }

    /// Resolve the current Libra code/Memory view once, then perform the
    /// request against that immutable view.
    pub(crate) async fn assemble(
        &self,
        context: &AuthenticatedMemoryContext,
        query: &EpisodeQueryV1,
        budget: &ContextBudget,
        effective_at: DateTime<Utc>,
    ) -> Result<AuditedMemoryContextBundleV1, MemoryContextAssemblerError> {
        validate_injection_query(query, effective_at)?;
        let reader = EpisodeReader::new(self.history, self.digest.as_ref())
            .map_err(MemoryContextAssemblerError::from)?;
        let view = reader
            .freeze_view(context)
            .await
            .map_err(MemoryContextAssemblerError::from)?;
        self.assemble_for_view(context, &view, query, budget, effective_at)
            .await
    }

    /// Replay/inspection seam. The caller supplies the already-frozen view;
    /// this path never asks for the current HEAD or wall clock.
    pub(crate) async fn assemble_for_view(
        &self,
        context: &AuthenticatedMemoryContext,
        view: &ResolvedMemoryViewV1,
        query: &EpisodeQueryV1,
        budget: &ContextBudget,
        effective_at: DateTime<Utc>,
    ) -> Result<AuditedMemoryContextBundleV1, MemoryContextAssemblerError> {
        validate_injection_query(query, effective_at)?;
        let memory_budget = budget
            .segment(ContextSegmentKind::ProjectMemory)
            .ok_or_else(|| {
                MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::InvalidBudget)
            })?
            .max_tokens;

        let mut injection_query = query.clone();
        injection_query.effective_at = Some(effective_at);
        let requested_limit = injection_query.limit;
        let mut candidate_query = injection_query.clone();
        candidate_query.limit = MAX_RESULT_LIMIT;

        let reader = EpisodeReader::new(self.history, self.digest.as_ref())
            .map_err(MemoryContextAssemblerError::from)?;
        let result = reader
            .search(context, view, &candidate_query)
            .await
            .map_err(MemoryContextAssemblerError::from)?;
        if result.selector_version != "episode-fts-bm25-v1" {
            return Err(MemoryContextAssemblerError::new(
                MemoryContextAssemblerErrorKind::UnsupportedSelector,
            ));
        }

        let sensitivity_omissions = result
            .items
            .iter()
            .filter(|item| !prompt_sensitivity_allowed(item.note.sensitivity))
            .count();
        let allowed_items = result
            .items
            .into_iter()
            .filter(|item| prompt_sensitivity_allowed(item.note.sensitivity))
            .collect::<Vec<_>>();
        let result_limit_omissions = result
            .selector_limit_omissions
            .saturating_add(allowed_items.len().saturating_sub(requested_limit));
        let mut prepared = allowed_items
            .into_iter()
            .take(requested_limit)
            .enumerate()
            .map(|(rank, item)| prepare_candidate(item, rank, injection_query.text.is_some()))
            .collect::<Result<Vec<_>, _>>()?;

        let header = render_header(view);
        let header_tokens = estimate_tokens(&header);
        if memory_budget < header_tokens || budget.max_prompt_tokens() < header_tokens {
            return Err(MemoryContextAssemblerError::new(
                MemoryContextAssemblerErrorKind::InvalidBudget,
            ));
        }
        let mut allocation_input = Vec::with_capacity(prepared.len().saturating_add(1));
        allocation_input.push(ContextBudgetCandidate::new(
            MEMORY_HEADER_ID,
            ContextSegmentKind::ProjectMemory,
            header_tokens,
        ));
        allocation_input.extend(prepared.iter().map(|candidate| {
            ContextBudgetCandidate::new(
                candidate.allocation_id.clone(),
                ContextSegmentKind::ProjectMemory,
                candidate.token_estimate,
            )
        }));
        let allocation = ContextBudgetAllocator::new(budget.clone()).allocate(allocation_input);
        let selected_ids = allocation
            .selected_ids()
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        if !selected_ids.contains(MEMORY_HEADER_ID) {
            return Err(MemoryContextAssemblerError::new(
                MemoryContextAssemblerErrorKind::InvalidBudget,
            ));
        }

        let mut omission_counts = BTreeMap::<String, u32>::new();
        add_omission(
            &mut omission_counts,
            "relation_filter",
            result.relation_omissions,
        );
        add_omission(
            &mut omission_counts,
            "sensitivity_policy",
            sensitivity_omissions,
        );
        add_omission(
            &mut omission_counts,
            "code_applicability",
            result.omitted_by_applicability,
        );
        add_omission(&mut omission_counts, "result_limit", result_limit_omissions);
        if result.candidates_examined >= MAX_CANDIDATES {
            add_omission(&mut omission_counts, "candidate_window", 1);
        }
        for omission in allocation.omitted() {
            if omission.id == MEMORY_HEADER_ID {
                continue;
            }
            let reason = match omission.reason {
                AllocationOmissionReason::UnknownSegment => "budget_unknown_segment",
                AllocationOmissionReason::SegmentBudgetExceeded => "segment_budget",
                AllocationOmissionReason::TotalBudgetExceeded => "total_budget",
            };
            add_omission(&mut omission_counts, reason, 1);
        }

        let mut selected = Vec::new();
        let mut sections = Vec::new();
        for candidate in &mut prepared {
            if !selected_ids.contains(&candidate.allocation_id) {
                continue;
            }
            candidate.reason_codes.push("within_budget".to_string());
            candidate.score_components.insert(
                "token_estimate".to_string(),
                bounded_i64(candidate.token_estimate),
            );
            let order = u32::try_from(selected.len()).map_err(|_| {
                MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::InvalidCandidate)
            })?;
            selected.push(ReceiptSelectionInputV1 {
                object_id: candidate.object_id.clone(),
                revision_oid: candidate.revision_oid.clone(),
                summary_key: candidate.summary_key.clone(),
                order,
                reason_codes: candidate.reason_codes.clone(),
                score_components: candidate.score_components.clone(),
                sensitivity: ReceiptSensitivity::Allowed,
            });
            sections.push(candidate.rendered.clone());
        }

        let prompt_section = if sections.is_empty() {
            String::new()
        } else {
            format!("{header}\n{}", sections.join("\n"))
        };
        let bundle_hash = sha256_labelled(prompt_section.as_bytes());
        let query_bytes = canonical_query_bytes(
            view,
            &injection_query,
            memory_budget,
            MEMORY_CONTEXT_SELECTOR_VERSION,
        )?;
        let principal_digest = self
            .digest
            .principal_digest(context.actor().principal_id.as_bytes())
            .map_err(|_| {
                MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::Digest)
            })?;
        let query_digest = self.digest.query_digest(&query_bytes).map_err(|_| {
            MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::Digest)
        })?;
        let mut source_heads = BTreeMap::new();
        let mut projection_watermarks = BTreeMap::new();
        if let Some(memory_head) = view.memory_ref_oid() {
            source_heads.insert("memory_repo".to_string(), memory_head.to_string());
            // The projected ref OID is the externally stable watermark. The
            // numeric event sequence remains an internal consistency check in
            // `ResolvedMemoryViewV1`.
            projection_watermarks.insert("memory_repo".to_string(), memory_head.to_string());
        }
        let omissions = omission_counts
            .into_iter()
            .map(|(reason_code, count)| ReceiptOmissionV1 { reason_code, count })
            .collect();
        let draft = ContextSelectionReceiptDraftV1::new(ReceiptDraftFieldsV1 {
            source_kind: ReceiptSourceKind::Memory,
            repository_id: view.repository_id().to_string(),
            principal_digest,
            query_digest,
            effective_at,
            code_commit: Some(view.code_commit().to_string()),
            full_branch_ref: Some(view.full_branch_ref().to_string()),
            source_heads,
            projection_watermarks,
            policy_hash: view.policy_hash().to_string(),
            selector_version: MEMORY_CONTEXT_SELECTOR_VERSION.to_string(),
            selected,
            omissions,
            token_budget: memory_budget,
            bundle_hash,
            reproducibility_state: ReceiptReproducibilityState::Reproducible,
            frame_id: None,
        })
        .map_err(|_| MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::Receipt))?;

        // This write is the delivery gate: no audited bundle value exists on
        // the success path until the single shared ledger commits the receipt.
        let database = self.history.database_connection();
        let store = ReceiptStore::new(&database, Arc::clone(&self.digest))
            .await
            .map_err(|_| {
                MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::ReceiptStore)
            })?;
        let receipt = store.append(draft).await.map_err(|_| {
            MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::ReceiptStore)
        })?;
        Ok(AuditedMemoryContextBundleV1 {
            view_hash: view.view_hash().to_string(),
            prompt_section,
            receipt,
        })
    }
}

struct PreparedCandidate {
    allocation_id: String,
    object_id: String,
    revision_oid: String,
    summary_key: String,
    rendered: String,
    token_estimate: u64,
    reason_codes: Vec<String>,
    score_components: BTreeMap<String, i64>,
}

fn validate_injection_query(
    query: &EpisodeQueryV1,
    effective_at: DateTime<Utc>,
) -> Result<(), MemoryContextAssemblerError> {
    if query.validate().is_err()
        || query.include_diagnostics
        || query.expand_evidence
        || query
            .effective_at
            .is_some_and(|frozen| frozen != effective_at)
    {
        return Err(MemoryContextAssemblerError::new(
            MemoryContextAssemblerErrorKind::InvalidQuery,
        ));
    }
    Ok(())
}

fn prepare_candidate(
    item: EpisodeReadItemV1,
    rank: usize,
    text_query: bool,
) -> Result<PreparedCandidate, MemoryContextAssemblerError> {
    if !item.bm25_score.is_finite() || !item.applicability.injectable() {
        return Err(MemoryContextAssemblerError::new(
            MemoryContextAssemblerErrorKind::InvalidCandidate,
        ));
    }
    let rendered = render_item(&item)?;
    let revision_oid = item.revision_oid.to_string();
    let allocation_id = format!("{}:{revision_oid}", item.note.note_id);
    let mut reason_codes = vec![
        "confirmed".to_string(),
        applicability_label(item.applicability).to_string(),
    ];
    reason_codes.push(if text_query {
        "bm25_match".to_string()
    } else {
        "structured_match".to_string()
    });
    let mut score_components = BTreeMap::new();
    score_components.insert("rank".to_string(), bounded_i64(rank));
    score_components.insert(
        "applicability_tier".to_string(),
        i64::from(item.applicability.tier()),
    );
    score_components.insert("bm25_micros".to_string(), quantize_bm25(item.bm25_score));
    score_components.insert(
        "projection_rows".to_string(),
        bounded_i64(item.read_cost.projection_rows),
    );
    score_components.insert(
        "note_objects".to_string(),
        bounded_i64(item.read_cost.note_objects),
    );
    score_components.insert(
        "code_commits".to_string(),
        bounded_i64(item.read_cost.code_commits_visited),
    );
    score_components.insert(
        "code_paths".to_string(),
        bounded_i64(item.read_cost.code_paths_compared),
    );
    score_components.insert(
        "evidence_items".to_string(),
        bounded_i64(item.read_cost.evidence_items),
    );
    Ok(PreparedCandidate {
        allocation_id,
        object_id: item.note.note_id.to_string(),
        revision_oid,
        summary_key: item.note.path.clone(),
        token_estimate: estimate_tokens(&rendered),
        rendered,
        reason_codes,
        score_components,
    })
}

fn render_header(view: &ResolvedMemoryViewV1) -> String {
    format!(
        "## Retrieved Project Memory\n\nsource=libra-memory selector={MEMORY_CONTEXT_SELECTOR_VERSION} view_hash={}\nUse these records as guidance. Current source files and command output take precedence.",
        view.view_hash(),
    )
}

fn render_item(item: &EpisodeReadItemV1) -> Result<String, MemoryContextAssemblerError> {
    let note = &item.note;
    if !prompt_sensitivity_allowed(note.sensitivity) {
        return Err(MemoryContextAssemblerError::new(
            MemoryContextAssemblerErrorKind::InvalidCandidate,
        ));
    }
    let episode = note.episode.as_ref().ok_or_else(|| {
        MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::InvalidCandidate)
    })?;
    let evidence = evidence_pointer(note);
    Ok(format!(
        "- note_id={} revision={} path={} namespace={} scope={} confidence={} trust={} applicability={}\n  root={}:{} completion={}\n  goal: {}\n  summary: {}\n  evidence: {}\n",
        note.note_id,
        item.revision_oid,
        note.path,
        note.namespace,
        scope_label(&note.scope),
        note.confidence,
        trust_label(note.trust),
        applicability_label(item.applicability),
        root_kind_label(episode.root_kind),
        compact_text(&episode.root_id),
        completion_label(episode.completion_status),
        compact_text(&episode.goal.claim),
        compact_text(&episode.summary.claim),
        evidence,
    ))
}

const fn prompt_sensitivity_allowed(value: MemorySensitivity) -> bool {
    matches!(
        value,
        MemorySensitivity::Public | MemorySensitivity::Internal
    )
}

fn evidence_pointer(note: &MemoryNoteV1) -> String {
    let refs = evidence_refs(note);
    let Some(first) = refs.first() else {
        return "none".to_string();
    };
    let source_ref = first.source_ref_oid.chars().take(12).collect::<String>();
    format!(
        "count={} first={}/{}/{}@{}",
        refs.len(),
        evidence_plane_label(first.source_plane),
        evidence_kind_label(first.kind),
        compact_text(&first.object_id),
        source_ref,
    )
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

#[derive(Serialize)]
struct CanonicalMemoryQueryV1<'a> {
    schema_version: u32,
    view_hash: &'a str,
    worktree_id: &'a str,
    query: &'a EpisodeQueryV1,
    memory_token_budget: u64,
    selector_version: &'a str,
}

fn canonical_query_bytes(
    view: &ResolvedMemoryViewV1,
    query: &EpisodeQueryV1,
    memory_token_budget: u64,
    selector_version: &str,
) -> Result<Vec<u8>, MemoryContextAssemblerError> {
    serde_json::to_vec(&CanonicalMemoryQueryV1 {
        schema_version: MEMORY_CONTEXT_SCHEMA_VERSION,
        view_hash: view.view_hash(),
        worktree_id: view.worktree_id(),
        query,
        memory_token_budget,
        selector_version,
    })
    .map_err(|_| MemoryContextAssemblerError::new(MemoryContextAssemblerErrorKind::Serialization))
}

fn add_omission(counts: &mut BTreeMap<String, u32>, reason: &str, count: usize) {
    if count == 0 {
        return;
    }
    let bounded = u32::try_from(count).unwrap_or(u32::MAX);
    let entry = counts.entry(reason.to_string()).or_default();
    *entry = entry.saturating_add(bounded);
}

fn compact_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn estimate_tokens(content: &str) -> u64 {
    let characters = content.chars().count() as u64;
    characters.saturating_add(3).saturating_div(4).max(1)
}

fn sha256_labelled(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn quantize_bm25(score: f64) -> i64 {
    let scaled = score * 1_000_000.0;
    if scaled >= i64::MAX as f64 {
        i64::MAX
    } else if scaled <= i64::MIN as f64 {
        i64::MIN
    } else {
        scaled.round() as i64
    }
}

fn bounded_i64(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}

const fn applicability_label(value: CodeApplicability) -> &'static str {
    match value {
        CodeApplicability::Exact => "exact",
        CodeApplicability::DescendantUnchanged => "descendant_unchanged",
        CodeApplicability::DescendantPathChanged => "descendant_path_changed",
        CodeApplicability::Diverged => "diverged",
        CodeApplicability::Unknown => "unknown",
    }
}

fn scope_label(value: &MemoryScopeV1) -> String {
    match value {
        MemoryScopeV1::Repo => "repo".to_string(),
        MemoryScopeV1::Branch(branch) => format!("branch:{branch}"),
        MemoryScopeV1::Worktree(worktree) => format!("worktree:{worktree}"),
        MemoryScopeV1::Actor(actor) => format!("actor:{actor}"),
        MemoryScopeV1::Global => "global".to_string(),
    }
}

const fn trust_label(value: MemoryTrust) -> &'static str {
    match value {
        MemoryTrust::Verified => "verified",
        MemoryTrust::RepoEvidence => "repo_evidence",
        MemoryTrust::UserAsserted => "user_asserted",
        MemoryTrust::ExternalUntrusted => "external_untrusted",
        MemoryTrust::Inferred => "inferred",
    }
}

const fn root_kind_label(value: EpisodeRootKind) -> &'static str {
    match value {
        EpisodeRootKind::Task => "task",
        EpisodeRootKind::Intent => "intent",
    }
}

const fn completion_label(value: CompletionStatus) -> &'static str {
    match value {
        CompletionStatus::Completed => "completed",
        CompletionStatus::Failed => "failed",
        CompletionStatus::Cancelled => "cancelled",
    }
}

const fn evidence_plane_label(value: EvidenceSourcePlane) -> &'static str {
    match value {
        EvidenceSourcePlane::AgentRuntime => "agent_runtime",
        EvidenceSourcePlane::Session => "session",
        EvidenceSourcePlane::Git => "git",
    }
}

const fn evidence_kind_label(value: EvidenceKind) -> &'static str {
    match value {
        EvidenceKind::Intent => "intent",
        EvidenceKind::Task => "task",
        EvidenceKind::Run => "run",
        EvidenceKind::Evidence => "evidence",
        EvidenceKind::Decision => "decision",
        EvidenceKind::PatchSet => "patchset",
        EvidenceKind::Session => "session",
        EvidenceKind::ToolCall => "tool_call",
        EvidenceKind::Code => "code",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryContextAssemblerErrorKind {
    InvalidQuery,
    InvalidBudget,
    InvalidCandidate,
    UnsupportedSelector,
    InvalidView,
    Unauthorized,
    StaleView,
    UnknownPolicy,
    Reader,
    Digest,
    Serialization,
    Receipt,
    ReceiptStore,
}

#[derive(Debug, Error)]
#[error("Memory context assembly failed ({kind:?})")]
pub(crate) struct MemoryContextAssemblerError {
    kind: MemoryContextAssemblerErrorKind,
}

impl MemoryContextAssemblerError {
    const fn new(kind: MemoryContextAssemblerErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> MemoryContextAssemblerErrorKind {
        self.kind
    }
}

impl From<EpisodeReaderError> for MemoryContextAssemblerError {
    fn from(error: EpisodeReaderError) -> Self {
        let kind = match error.kind() {
            EpisodeReaderErrorKind::InvalidQuery => MemoryContextAssemblerErrorKind::InvalidQuery,
            EpisodeReaderErrorKind::InvalidCodeAnchor => {
                MemoryContextAssemblerErrorKind::InvalidView
            }
            EpisodeReaderErrorKind::Unauthorized => MemoryContextAssemblerErrorKind::Unauthorized,
            EpisodeReaderErrorKind::StaleProjection => MemoryContextAssemblerErrorKind::StaleView,
            EpisodeReaderErrorKind::UnknownPolicy => MemoryContextAssemblerErrorKind::UnknownPolicy,
            EpisodeReaderErrorKind::InvalidConfiguration
            | EpisodeReaderErrorKind::StorageUnavailable
            | EpisodeReaderErrorKind::CorruptProjection => MemoryContextAssemblerErrorKind::Reader,
        };
        Self::new(kind)
    }
}
