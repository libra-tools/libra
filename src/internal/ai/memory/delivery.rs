//! Model-free, audited Memory delivery for one authenticated Agent request.

use std::{collections::BTreeSet, sync::Arc};

use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use super::{AuthenticatedMemoryContext, EpisodeQueryV1};
use crate::{
    internal::ai::{
        context_budget::{
            ContextBudget, ContextSegmentBudget, ContextSegmentKind, TruncationPolicy,
            memory::{MemoryContextAssembler, MemoryContextAssemblerErrorKind},
        },
        history::HistoryManager,
        keyed_digest::RepositoryKeyedDigest,
    },
    utils::util::DATABASE,
};

const MAX_RECALL_QUERY_BYTES: usize = 4 * 1024;
const MAX_RECALL_QUERY_TERM_BYTES: usize = 256;
const MAX_RECALL_QUERY_TERMS: usize = 32;
const BRIDGE_MEMORY_TOKEN_BUDGET: u64 = 1_600;

/// Delivery metadata that is only constructed after its ContextReceipt is
/// durably committed by [`MemoryContextAssembler`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuditedMemoryDeliveryV1 {
    prompt_section: String,
    receipt_id: Uuid,
    view_hash: String,
    bundle_hash: String,
    selected_count: usize,
    token_budget: u64,
}

impl AuditedMemoryDeliveryV1 {
    pub(crate) fn prompt_section(&self) -> &str {
        &self.prompt_section
    }

    pub(crate) const fn receipt_id(&self) -> Uuid {
        self.receipt_id
    }

    pub(crate) fn view_hash(&self) -> &str {
        &self.view_hash
    }

    pub(crate) fn bundle_hash(&self) -> &str {
        &self.bundle_hash
    }

    pub(crate) const fn selected_count(&self) -> usize {
        self.selected_count
    }

    pub(crate) const fn token_budget(&self) -> u64 {
        self.token_budget
    }
}

/// Shared model-free owner for bounded Memory queries and audited delivery.
pub(crate) struct AuditedMemoryDelivery {
    history: Arc<HistoryManager>,
    digest: Arc<RepositoryKeyedDigest>,
    budget: ContextBudget,
}

impl AuditedMemoryDelivery {
    pub(crate) async fn open(
        history: Arc<HistoryManager>,
    ) -> Result<Self, AuditedMemoryDeliveryError> {
        let database_path = history.repository_path().join(DATABASE);
        let database = history.database_connection();
        let digest =
            RepositoryKeyedDigest::load_existing_with_connection(&database_path, &database)
                .await
                .map_err(|_| {
                    AuditedMemoryDeliveryError::new(AuditedMemoryDeliveryErrorKind::Digest)
                })?;
        let budget = ContextBudget::from_segments(
            BRIDGE_MEMORY_TOKEN_BUDGET,
            vec![ContextSegmentBudget::new(
                ContextSegmentKind::ProjectMemory,
                BRIDGE_MEMORY_TOKEN_BUDGET,
                TruncationPolicy::OldestFirst,
            )],
        )
        .map_err(|_| AuditedMemoryDeliveryError::new(AuditedMemoryDeliveryErrorKind::Contract))?;
        Ok(Self::from_dependencies(history, digest, budget))
    }

    pub(crate) fn from_dependencies(
        history: Arc<HistoryManager>,
        digest: Arc<RepositoryKeyedDigest>,
        budget: ContextBudget,
    ) -> Self {
        Self {
            history,
            digest,
            budget,
        }
    }

    pub(crate) async fn recall(
        &self,
        context: &AuthenticatedMemoryContext,
        input: &str,
    ) -> Result<Option<AuditedMemoryDeliveryV1>, AuditedMemoryDeliveryError> {
        let Some(text) = bounded_recall_text_v1(input) else {
            return Ok(None);
        };
        let query = EpisodeQueryV1 {
            text: Some(text),
            ..EpisodeQueryV1::default()
        };

        for attempt in 0..=1 {
            match MemoryContextAssembler::new(self.history.as_ref(), Arc::clone(&self.digest))
                .assemble(context, &query, &self.budget, Utc::now())
                .await
            {
                Ok(bundle) => {
                    let receipt = bundle.receipt();
                    return Ok(Some(AuditedMemoryDeliveryV1 {
                        prompt_section: bundle.prompt_section().to_string(),
                        receipt_id: receipt.receipt_id(),
                        view_hash: bundle.view_hash().to_string(),
                        bundle_hash: receipt.bundle_hash().to_string(),
                        selected_count: receipt.selected().len(),
                        token_budget: receipt.token_budget(),
                    }));
                }
                Err(error)
                    if error.kind() == MemoryContextAssemblerErrorKind::StaleView
                        && attempt == 0 =>
                {
                    continue;
                }
                Err(error) => return Err(AuditedMemoryDeliveryError::from(error.kind())),
            }
        }

        Err(AuditedMemoryDeliveryError::new(
            AuditedMemoryDeliveryErrorKind::ProjectionStale,
        ))
    }
}

/// Reduce an arbitrary Agent request to the bounded plain-text query accepted
/// by the FTS5 reader. Input order is retained and duplicate terms use ASCII
/// case-folding, matching the reader contract.
pub(crate) fn bounded_recall_text_v1(input: &str) -> Option<String> {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut bytes = 0usize;
    for term in input.split(|character: char| !character.is_alphanumeric()) {
        if term.is_empty() || term.len() > MAX_RECALL_QUERY_TERM_BYTES {
            continue;
        }
        let key = term.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let separator = usize::from(!selected.is_empty());
        if selected.len() == MAX_RECALL_QUERY_TERMS
            || bytes.saturating_add(separator).saturating_add(term.len()) > MAX_RECALL_QUERY_BYTES
        {
            break;
        }
        bytes = bytes.saturating_add(separator).saturating_add(term.len());
        selected.push(term);
    }
    (!selected.is_empty()).then(|| selected.join(" "))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuditedMemoryDeliveryErrorKind {
    Digest,
    Contract,
    Policy,
    Corrupt,
    Storage,
    ProjectionStale,
}

impl AuditedMemoryDeliveryErrorKind {
    pub(crate) const fn stable_code(self) -> &'static str {
        match self {
            Self::Digest => "LBR-MEMORY-001",
            Self::Contract => "LBR-MEMORY-002",
            Self::Policy => "LBR-MEMORY-003",
            Self::Corrupt => "LBR-MEMORY-004",
            Self::Storage => "LBR-MEMORY-005",
            Self::ProjectionStale => "LBR-MEMORY-PROJECTION-STALE",
        }
    }

    pub(crate) const fn retryable(self) -> bool {
        matches!(self, Self::Storage | Self::ProjectionStale)
    }
}

#[derive(Debug, Error)]
#[error("audited Memory delivery failed ({kind:?})")]
pub(crate) struct AuditedMemoryDeliveryError {
    kind: AuditedMemoryDeliveryErrorKind,
}

impl AuditedMemoryDeliveryError {
    const fn new(kind: AuditedMemoryDeliveryErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> AuditedMemoryDeliveryErrorKind {
        self.kind
    }
}

impl From<MemoryContextAssemblerErrorKind> for AuditedMemoryDeliveryError {
    fn from(kind: MemoryContextAssemblerErrorKind) -> Self {
        let mapped = match kind {
            MemoryContextAssemblerErrorKind::Digest => AuditedMemoryDeliveryErrorKind::Digest,
            MemoryContextAssemblerErrorKind::InvalidQuery
            | MemoryContextAssemblerErrorKind::InvalidBudget
            | MemoryContextAssemblerErrorKind::UnsupportedSelector
            | MemoryContextAssemblerErrorKind::ReaderConfiguration
            | MemoryContextAssemblerErrorKind::Serialization
            | MemoryContextAssemblerErrorKind::Receipt => AuditedMemoryDeliveryErrorKind::Contract,
            MemoryContextAssemblerErrorKind::Unauthorized
            | MemoryContextAssemblerErrorKind::UnknownPolicy => {
                AuditedMemoryDeliveryErrorKind::Policy
            }
            MemoryContextAssemblerErrorKind::StaleView => {
                AuditedMemoryDeliveryErrorKind::ProjectionStale
            }
            MemoryContextAssemblerErrorKind::ReceiptStore => {
                AuditedMemoryDeliveryErrorKind::Storage
            }
            MemoryContextAssemblerErrorKind::ReaderStorage => {
                AuditedMemoryDeliveryErrorKind::Storage
            }
            MemoryContextAssemblerErrorKind::InvalidCandidate
            | MemoryContextAssemblerErrorKind::InvalidView
            | MemoryContextAssemblerErrorKind::ReaderCorrupt => {
                AuditedMemoryDeliveryErrorKind::Corrupt
            }
        };
        Self::new(mapped)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sea_orm::{ConnectionTrait, Statement};
    use sha2::{Digest, Sha256};

    use super::{AuditedMemoryDelivery, AuditedMemoryDeliveryErrorKind, AuditedMemoryDeliveryV1};
    use crate::internal::ai::{
        context_budget::ContextBudget,
        memory::{
            AuthenticatedMemoryContext, MemorySensitivity,
            domain::{ActorKind, ActorRefV1},
            reader::tests::{commit_injectable_episode, history, seed_code_head},
            writer::tests::fixture,
        },
    };

    async fn receipt_count(database: &sea_orm::DatabaseConnection) -> i64 {
        database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS n FROM context_selection_receipt".to_string(),
            ))
            .await
            .expect("count receipts")
            .expect("count row")
            .try_get::<i64>("", "n")
            .expect("receipt count")
    }

    fn assert_delivery_contract(delivery: &AuditedMemoryDeliveryV1) {
        assert_eq!(delivery.token_budget(), 1_600);
        assert!(delivery.view_hash().starts_with("sha256:"));
        assert_eq!(
            delivery.bundle_hash(),
            format!(
                "sha256:{}",
                hex::encode(Sha256::digest(delivery.prompt_section().as_bytes()))
            )
        );
        assert!(!delivery.receipt_id().is_nil());
    }

    #[test]
    fn stable_error_contract_is_complete_and_retryability_is_pinned() {
        for (kind, code, retryable) in [
            (
                AuditedMemoryDeliveryErrorKind::Digest,
                "LBR-MEMORY-001",
                false,
            ),
            (
                AuditedMemoryDeliveryErrorKind::Contract,
                "LBR-MEMORY-002",
                false,
            ),
            (
                AuditedMemoryDeliveryErrorKind::Policy,
                "LBR-MEMORY-003",
                false,
            ),
            (
                AuditedMemoryDeliveryErrorKind::Corrupt,
                "LBR-MEMORY-004",
                false,
            ),
            (
                AuditedMemoryDeliveryErrorKind::Storage,
                "LBR-MEMORY-005",
                true,
            ),
            (
                AuditedMemoryDeliveryErrorKind::ProjectionStale,
                "LBR-MEMORY-PROJECTION-STALE",
                true,
            ),
        ] {
            assert_eq!(kind.stable_code(), code);
            assert_eq!(kind.retryable(), retryable);
        }
    }

    #[tokio::test]
    async fn agent_recall_returns_receipted_delivery_and_zero_hit_is_still_a_delivery() {
        let fixture = fixture().await;
        let code_commit = seed_code_head(&fixture).await;
        commit_injectable_episode(
            &fixture,
            code_commit,
            "task-dsh-recall",
            1,
            MemorySensitivity::Internal,
            "dshuniquerecalltoken",
            "The DSH adapter must receive this exact audited Memory candidate.",
        )
        .await;
        let delivery = AuditedMemoryDelivery::from_dependencies(
            Arc::new(history(&fixture)),
            Arc::clone(&fixture.digest),
            ContextBudget::default(),
        );
        let context = AuthenticatedMemoryContext::new(
            fixture.digest.repository_id(),
            ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "deepseek-harness:dsh-session".to_string(),
            },
        )
        .expect("authenticated DSH agent");

        let selected = delivery
            .recall(&context, "dshuniquerecalltoken")
            .await
            .expect("recall")
            .expect("searchable query returns a delivery");
        assert_delivery_contract(&selected);
        assert_eq!(selected.selected_count(), 1);
        assert!(selected.prompt_section().contains("dshuniquerecalltoken"));

        let empty = delivery
            .recall(&context, "termwithnomemorycandidate")
            .await
            .expect("zero-hit recall")
            .expect("zero-hit query remains auditable");
        assert_delivery_contract(&empty);
        assert_eq!(empty.selected_count(), 0);
        assert_eq!(empty.prompt_section(), "");
        assert_eq!(receipt_count(fixture.database.as_ref()).await, 2);
    }

    #[tokio::test]
    async fn punctuation_only_query_returns_none_without_writing_a_receipt() {
        let fixture = fixture().await;
        let delivery = AuditedMemoryDelivery::from_dependencies(
            Arc::new(history(&fixture)),
            Arc::clone(&fixture.digest),
            ContextBudget::default(),
        );
        let context = AuthenticatedMemoryContext::new(
            fixture.digest.repository_id(),
            ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "deepseek-harness:no-query".to_string(),
            },
        )
        .expect("authenticated DSH agent");

        assert!(
            delivery
                .recall(&context, "---\n\t")
                .await
                .expect("bounded query")
                .is_none()
        );
        assert_eq!(receipt_count(fixture.database.as_ref()).await, 0);
    }
}
