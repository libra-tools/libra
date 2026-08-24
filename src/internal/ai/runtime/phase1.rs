//! Phase 1 Plan — formal write helpers.
//!
//! The Code UI Phase Workflow models Phase 1 as the **Plan** phase: the
//! Phase 0 `IntentSpec` gets compiled into an `ExecutionPlanSpec` which is
//! persisted as a paired execution / test plan revision and then folded into
//! the scheduler state machine.
//!
//! # Runtime-owned contract, transitional storage
//!
//! [`PlanWriteOutcome`] and [`write_plan_set`] are the Runtime-owned Phase 1
//! contract surface. `write_plan_set` currently delegates into
//! [`crate::internal::ai::orchestrator::persistence::write_plan_set_with_outcome`]
//! so the existing `PersistedPlanRevision` / step-id plumbing stays in the
//! orchestrator persistence layer while provider/UI callers target the Runtime
//! entry point. Once that storage code is folded into this module, callers keep
//! the same signature and outcome type.
//!
//! The important invariant is that Phase 1 always writes an execution/test plan
//! pair and returns scheduler-facing IDs for both plans; callers must not fall
//! back to a single-plan write path.

use std::io::Read;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::worker::{
    InteractionResponse, RuntimeExecutionContext, RuntimeInteractionDelivery, RuntimeTurnExecution,
    RuntimeWorkerError, TurnRequest,
};
use crate::internal::ai::agent::ToolLoopConfig;

pub const MAX_PHASE1_DURABLE_BYTES: usize = 8 * 1024 * 1024;
const PHASE1_ID_SERIALIZATION_RESERVE: usize = 4 * 1024;
const MAX_PHASE1_CONTEXT_FILES: usize = 64;
const MAX_PHASE1_CONTEXT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

/// Durable, UI-neutral payload required to reconstruct a Phase 1 review gate.
///
/// The Code UI snapshot is only a projection.  Recovery loads this payload
/// from the session root and then re-registers the runtime-owned interaction;
/// Web adapters must not keep a private `PendingPlan` state machine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phase1ReviewContext {
    pub schema_version: u32,
    pub interaction_id: String,
    pub intent_id: String,
    pub intent_spec_id: String,
    pub persisted_plan: Phase1PersistedPlan,
    pub intent_spec: crate::internal::ai::intentspec::IntentSpec,
    pub plan_draft: crate::internal::ai::tools::context::SubmitPlanDraftArgs,
    pub execution_plan: crate::internal::ai::orchestrator::types::ExecutionPlanSpec,
    pub default_allow_network: bool,
    pub checkout: Phase1CheckoutBinding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phase1CheckoutBinding {
    pub canonical_working_dir: String,
    #[serde(default)]
    pub repo_id: String,
    pub repo_locator: String,
    pub base_ref: String,
    #[serde(default)]
    pub workspace_fingerprint: String,
    /// Fast drift hint used for projections and pre-write retries. Execute
    /// authorization always compares the content fingerprint above.
    #[serde(default)]
    pub workspace_change_token: String,
    pub head_oid: Option<String>,
    pub branch_label: String,
    pub worktree_id: Option<String>,
}

const PHASE1_WORKSPACE_METADATA_FINGERPRINT_PREFIX: &str = "metadata-v1:";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Phase1CheckoutIdentity {
    canonical_working_dir: String,
    repo_id: String,
    repo_locator: String,
    base_ref: String,
    head_oid: Option<String>,
    branch_label: String,
    worktree_id: Option<String>,
}

impl From<&Phase1CheckoutBinding> for Phase1CheckoutIdentity {
    fn from(binding: &Phase1CheckoutBinding) -> Self {
        Self {
            canonical_working_dir: binding.canonical_working_dir.clone(),
            repo_id: binding.repo_id.clone(),
            repo_locator: binding.repo_locator.clone(),
            base_ref: binding.base_ref.clone(),
            head_oid: binding.head_oid.clone(),
            branch_label: binding.branch_label.clone(),
            worktree_id: binding.worktree_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Phase1PersistedPlan {
    Persisted {
        execution_plan_id: String,
        test_plan_id: String,
    },
    #[default]
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phase1StartSeed {
    pub schema_version: u32,
    /// Stable identity for this persisted attempt. Server-generated commands
    /// use it to distinguish a determinate pre-write retry from the prior
    /// Failed command while crash recovery reuses the same value.
    #[serde(default)]
    pub attempt_id: String,
    pub source_interaction_id: String,
    pub intent_id: String,
    pub intent_spec_id: String,
    pub intent_spec_json: String,
    pub source_resolution: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_note: Option<String>,
    pub checkout: Phase1CheckoutBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_plan: Option<crate::internal::ai::orchestrator::types::ExecutionPlanSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_plan_id: Option<String>,
    #[serde(default)]
    pub prior_persisted_plan: Phase1PersistedPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_command_id: Option<String>,
}

impl Phase1StartSeed {
    pub const SCHEMA_VERSION: u32 = 3;

    pub fn durable_digest(&self) -> std::io::Result<String> {
        let bytes = serde_json::to_vec(self).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Phase 1 start seed cannot be canonicalized: {error}"),
            )
        })?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase1RetryIntentReviewState {
    NoIntent,
    PendingTerminal,
    TerminalWithoutRetry,
    Open(crate::internal::ai::session::Phase1RetryIntentReview),
    Resolved {
        review: crate::internal::ai::session::Phase1RetryIntentReview,
        resolution: String,
    },
}

fn validate_phase1_retry_intent_review_shape(
    review: &crate::internal::ai::session::Phase1RetryIntentReview,
    phase1_turn_id: &str,
) -> std::io::Result<()> {
    let digest_valid = review.start_seed_digest.len() == 64
        && review
            .start_seed_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit());
    if review.interaction_id.trim().is_empty()
        || review.intent_id.trim().is_empty()
        || review.intent_spec_id.trim().is_empty()
        || review.source_interaction_id.trim().is_empty()
        || review.interaction_id == review.source_interaction_id
        || !review.source_resolution.eq_ignore_ascii_case("confirm")
        || review.source_phase1_turn_id != phase1_turn_id
        || !digest_valid
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 retry Intent review for command '{phase1_turn_id}' has invalid or mismatched lineage"
            ),
        ));
    }
    Ok(())
}

pub fn validate_phase1_retry_intent_review_for_seed(
    review: &crate::internal::ai::session::Phase1RetryIntentReview,
    phase1_turn_id: &str,
    seed: &Phase1StartSeed,
) -> std::io::Result<()> {
    validate_phase1_retry_intent_review_shape(review, phase1_turn_id)?;
    if review.intent_id != seed.intent_id
        || review.intent_spec_id != seed.intent_spec_id
        || review.source_interaction_id != seed.source_interaction_id
        || !review
            .source_resolution
            .eq_ignore_ascii_case(&seed.source_resolution)
        || review.start_seed_digest != seed.durable_digest()?
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 retry Intent review for command '{phase1_turn_id}' does not match its durable start seed"
            ),
        ));
    }
    Ok(())
}

pub fn phase1_source_resolution_matches_seed<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
    seed: &Phase1StartSeed,
) -> bool {
    use crate::internal::ai::session::CodeWorkflowEventKind;

    let mut latest = None;
    for event in events {
        match event {
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                ..
            } if interaction_id == &seed.source_interaction_id => {
                latest = Some(resolution.as_str());
            }
            _ => {}
        }
    }
    latest.is_some_and(|resolution| resolution.eq_ignore_ascii_case(&seed.source_resolution))
}

/// Inspect the durable lifecycle of one Phase 1 command and its atomically
/// embedded retry gate. The state distinguishes a not-yet-admitted handoff
/// from a pending terminal so Cancel can wait only when a terminal writer
/// really exists.
pub fn phase1_retry_intent_review_state<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
    phase1_turn_id: &str,
) -> std::io::Result<Phase1RetryIntentReviewState> {
    use std::collections::HashMap;

    use crate::internal::ai::session::CodeWorkflowEventKind;

    let mut saw_intent = false;
    let mut terminal: Option<Option<crate::internal::ai::session::Phase1RetryIntentReview>> = None;
    let mut resolutions = HashMap::<String, String>::new();
    for event in events {
        match event {
            CodeWorkflowEventKind::CommandIntentPersisted { command }
                if command.identity.command_id == phase1_turn_id =>
            {
                saw_intent = true;
            }
            CodeWorkflowEventKind::CommandTerminalFailure {
                command,
                retry_intent_review,
                ..
            } if command.command_id == phase1_turn_id => {
                if terminal.is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Phase 1 command '{phase1_turn_id}' has conflicting terminal rows"),
                    ));
                }
                if let Some(review) = retry_intent_review.as_ref() {
                    validate_phase1_retry_intent_review_shape(review, phase1_turn_id)?;
                }
                terminal = Some(retry_intent_review.clone());
            }
            CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                ..
            }
            | CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                if command.command_id == phase1_turn_id =>
            {
                if terminal.replace(None).is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Phase 1 command '{phase1_turn_id}' has conflicting terminal rows"),
                    ));
                }
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                prior_interaction_resolutions,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                prior_interaction_resolutions,
                ..
            } => {
                for (resolved_id, resolved_as) in
                    prior_interaction_resolutions
                        .iter()
                        .chain(std::iter::once(&(
                            interaction_id.clone(),
                            resolution.clone(),
                        )))
                {
                    if resolutions
                        .insert(resolved_id.clone(), resolved_as.clone())
                        .is_some_and(|existing| existing != *resolved_as)
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "interaction '{resolved_id}' has conflicting durable resolutions"
                            ),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    if terminal.is_some() && !saw_intent {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Phase 1 command '{phase1_turn_id}' has a terminal row without durable intent"),
        ));
    }
    match terminal {
        Some(Some(review)) => match resolutions.remove(&review.interaction_id) {
            Some(resolution) => Ok(Phase1RetryIntentReviewState::Resolved { review, resolution }),
            None => Ok(Phase1RetryIntentReviewState::Open(review)),
        },
        Some(None) => Ok(Phase1RetryIntentReviewState::TerminalWithoutRetry),
        None if saw_intent => Ok(Phase1RetryIntentReviewState::PendingTerminal),
        None => Ok(Phase1RetryIntentReviewState::NoIntent),
    }
}

impl Phase1CheckoutBinding {
    pub fn same_intent_repository_as(&self, other: &Self) -> bool {
        self.repo_id == other.repo_id
            && self.repo_locator == other.repo_locator
            && self.base_ref == other.base_ref
    }

    fn validate_durable_shape(&self) -> std::io::Result<()> {
        use std::str::FromStr;

        use git_internal::hash::ObjectHash;

        let fingerprint_valid = self.workspace_fingerprint.len() == 64
            && self
                .workspace_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        let change_token_valid = self.workspace_change_token.is_empty()
            || self
                .workspace_change_token
                .strip_prefix(PHASE1_WORKSPACE_METADATA_FINGERPRINT_PREFIX)
                .is_some_and(|token| {
                    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
        let head_valid = match (
            self.branch_label.strip_prefix("detached:"),
            self.head_oid.as_deref(),
        ) {
            (Some(label_oid), Some(oid)) => label_oid == oid && ObjectHash::from_str(oid).is_ok(),
            (Some(_), None) => false,
            (None, Some(oid)) => ObjectHash::from_str(oid).is_ok(),
            // A named HEAD without a branch row is the normal unborn state
            // immediately after `libra init` and before the first commit.
            (None, None) => true,
        };
        if !std::path::Path::new(&self.canonical_working_dir).is_absolute()
            || self.repo_id.trim().is_empty()
            || self.repo_locator.trim().is_empty()
            || self.base_ref.trim().is_empty()
            || self.branch_label.trim().is_empty()
            || !fingerprint_valid
            || !change_token_valid
            || !head_valid
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Phase 1 checkout binding is incomplete or malformed",
            ));
        }
        Ok(())
    }

    pub async fn capture(
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
    ) -> std::io::Result<Self> {
        Self::capture_with_post_content_hook(working_dir, spec, || Ok(())).await
    }

    async fn capture_with_post_content_hook<F>(
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
        post_content_hook: F,
    ) -> std::io::Result<Self>
    where
        F: FnOnce() -> std::io::Result<()> + Send + 'static,
    {
        let identity = Self::capture_identity(working_dir, spec).await?;
        let fingerprint_root = std::path::PathBuf::from(&identity.canonical_working_dir);
        let (workspace_fingerprint, workspace_change_token) = tokio::task::spawn_blocking(move || {
            crate::internal::ai::workspace_snapshot::workspace_snapshot_stable_fingerprints_with_post_content_hook(
                &fingerprint_root,
                post_content_hook,
            )
        })
        .await
        .map_err(|error| {
            std::io::Error::other(format!("Phase 1 workspace fingerprint worker failed: {error}"))
        })??;
        let final_identity = Self::capture_identity(working_dir, spec).await?;
        if final_identity != identity {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "checkout identity changed while its Phase 1 fingerprints were captured; retry against a stable checkout",
            ));
        }
        let binding = Self {
            canonical_working_dir: identity.canonical_working_dir,
            repo_id: identity.repo_id,
            repo_locator: identity.repo_locator,
            base_ref: identity.base_ref,
            workspace_fingerprint,
            workspace_change_token: format!(
                "{PHASE1_WORKSPACE_METADATA_FINGERPRINT_PREFIX}{workspace_change_token}"
            ),
            head_oid: identity.head_oid,
            branch_label: identity.branch_label,
            worktree_id: identity.worktree_id,
        };
        binding.validate_durable_shape()?;
        Ok(binding)
    }

    async fn capture_identity(
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
    ) -> std::io::Result<Phase1CheckoutIdentity> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let canonical_working_dir = std::fs::canonicalize(working_dir)?;
        let request_scope = crate::internal::worktree_scope::RequestScope::try_resolve(
            canonical_working_dir.clone(),
        )?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Phase 1 working directory '{}' is not a Libra checkout",
                    canonical_working_dir.display()
                ),
            )
        })?;
        let worktree_id = request_scope.scope.worktree_id().map(str::to_string);
        let db = crate::internal::db::get_db_conn_instance_for_path(
            &request_scope.storage.join(crate::utils::util::DATABASE),
        )
        .await
        .map_err(|error| std::io::Error::other(format!("failed to open repo db: {error}")))?;
        let repo_id = crate::internal::workspace::RepoIdentity::resolve(&db)
            .await
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("failed to resolve Phase 1 repository identity: {error}"),
                )
            })?
            .as_str()
            .to_string();
        let mut head_query = crate::internal::model::reference::Entity::find()
            .filter(
                crate::internal::model::reference::Column::Kind
                    .eq(crate::internal::model::reference::ConfigKind::Head),
            )
            .filter(crate::internal::model::reference::Column::Remote.is_null());
        head_query = match &worktree_id {
            Some(id) => head_query
                .filter(crate::internal::model::reference::Column::WorktreeId.eq(id.clone())),
            None => {
                head_query.filter(crate::internal::model::reference::Column::WorktreeId.is_null())
            }
        };
        let head_rows = head_query
            .all(&db)
            .await
            .map_err(|error| std::io::Error::other(format!("failed to read HEAD: {error}")))?;
        if head_rows.len() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "expected one HEAD row for Phase 1 checkout, found {}",
                    head_rows.len()
                ),
            ));
        }
        let head = &head_rows[0];
        let (branch_label, head_oid) = if let Some(branch_name) = head.name.as_ref() {
            let branch_rows = crate::internal::model::reference::Entity::find()
                .filter(
                    crate::internal::model::reference::Column::Kind
                        .eq(crate::internal::model::reference::ConfigKind::Branch),
                )
                .filter(crate::internal::model::reference::Column::Remote.is_null())
                .filter(crate::internal::model::reference::Column::WorktreeId.is_null())
                .filter(crate::internal::model::reference::Column::Name.eq(branch_name.clone()))
                .all(&db)
                .await
                .map_err(|error| {
                    std::io::Error::other(format!("failed to read HEAD branch: {error}"))
                })?;
            match branch_rows.as_slice() {
                [] => (branch_name.clone(), None),
                [branch] => {
                    let oid = branch
                        .commit
                        .clone()
                        .filter(|oid| !oid.trim().is_empty())
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("HEAD branch '{branch_name}' is missing its commit id"),
                            )
                        })?;
                    (branch_name.clone(), Some(oid))
                }
                rows => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "expected at most one row for HEAD branch '{branch_name}', found {}",
                            rows.len()
                        ),
                    ));
                }
            }
        } else {
            let oid = head
                .commit
                .clone()
                .filter(|oid| !oid.trim().is_empty())
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "detached HEAD is missing its commit id",
                    )
                })?;
            (format!("detached:{oid}"), Some(oid))
        };
        Ok(Phase1CheckoutIdentity {
            canonical_working_dir: canonical_working_dir.to_string_lossy().into_owned(),
            repo_id,
            repo_locator: spec.metadata.target.repo.locator.clone(),
            base_ref: spec.metadata.target.base_ref.clone(),
            head_oid,
            branch_label,
            worktree_id,
        })
    }

    pub async fn validate_identity(
        &self,
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
    ) -> std::io::Result<()> {
        let current = Self::capture_identity(working_dir, spec).await?;
        if current != Phase1CheckoutIdentity::from(self) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Phase 1 review context belongs to a different checkout, HEAD, or base ref",
            ));
        }
        Ok(())
    }

    /// Check the Intent-level repository tuple while permitting an explicit
    /// Plan revision to move to a different HEAD/worktree in that repository.
    pub async fn validate_same_intent_repository(
        &self,
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
    ) -> std::io::Result<()> {
        let current = Self::capture_identity(working_dir, spec).await?;
        if current.repo_id != self.repo_id
            || current.repo_locator != self.repo_locator
            || current.base_ref != self.base_ref
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Phase 1 review context belongs to a different Intent repository or base ref",
            ));
        }
        Ok(())
    }

    pub async fn workspace_matches(&self, working_dir: &std::path::Path) -> std::io::Result<bool> {
        let canonical_working_dir = std::fs::canonicalize(working_dir)?;
        if canonical_working_dir.to_string_lossy() != self.canonical_working_dir {
            return Ok(false);
        }
        let fingerprint_root = canonical_working_dir;
        let current = tokio::task::spawn_blocking(move || {
            crate::internal::ai::workspace_snapshot::workspace_snapshot_stable_fingerprints(
                &fingerprint_root,
            )
            .map(|(content, _metadata)| content)
        })
        .await
        .map_err(|error| {
            std::io::Error::other(format!("Phase 1 workspace check worker failed: {error}"))
        })??;
        Ok(current == self.workspace_fingerprint)
    }

    /// Authorize an Execute boundary against one stable checkout identity and
    /// the exact content fingerprint. Identity is sampled on both sides of
    /// the stable workspace scan so a same-content HEAD/repository swap cannot
    /// cross the gate between two independent checks.
    pub async fn validate_exact(
        &self,
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
    ) -> std::io::Result<()> {
        self.validate_exact_with_post_scan_hook(working_dir, spec, || async { Ok(()) })
            .await
    }

    async fn validate_exact_with_post_scan_hook<F, Fut>(
        &self,
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
        post_scan_hook: F,
    ) -> std::io::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = std::io::Result<()>>,
    {
        self.validate_durable_shape()?;
        let identity_before = Self::capture_identity(working_dir, spec).await?;
        if identity_before != Phase1CheckoutIdentity::from(self) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Phase 1 review context belongs to a different checkout, HEAD, or base ref",
            ));
        }
        let fingerprint_root = std::path::PathBuf::from(&identity_before.canonical_working_dir);
        let current = tokio::task::spawn_blocking(move || {
            crate::internal::ai::workspace_snapshot::workspace_snapshot_stable_fingerprints(
                &fingerprint_root,
            )
            .map(|(content, _metadata)| content)
        })
        .await
        .map_err(|error| {
            std::io::Error::other(format!(
                "Phase 1 exact workspace check worker failed: {error}"
            ))
        })??;
        post_scan_hook().await?;
        let identity_after = Self::capture_identity(working_dir, spec).await?;
        if identity_after != identity_before || identity_after != Phase1CheckoutIdentity::from(self)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "checkout identity changed while the exact Phase 1 workspace was verified",
            ));
        }
        if current != self.workspace_fingerprint {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the exact workspace content changed after this Phase 1 review context was captured",
            ));
        }
        Ok(())
    }

    /// Compare the cheap change token used for UI warnings and determinate
    /// pre-write retries. Legacy contexts without the additive token fall back
    /// to their exact content fingerprint.
    pub async fn workspace_change_matches(
        &self,
        working_dir: &std::path::Path,
    ) -> std::io::Result<bool> {
        if self.workspace_change_token.is_empty() {
            return self.workspace_matches(working_dir).await;
        }
        let canonical_working_dir = std::fs::canonicalize(working_dir)?;
        if canonical_working_dir.to_string_lossy() != self.canonical_working_dir {
            return Ok(false);
        }
        let fingerprint_root = canonical_working_dir;
        let current = tokio::task::spawn_blocking(move || {
            crate::internal::ai::workspace_snapshot::workspace_snapshot_metadata_fingerprint(
                &fingerprint_root,
            )
        })
        .await
        .map_err(|error| {
            std::io::Error::other(format!("Phase 1 workspace check worker failed: {error}"))
        })??;
        let expected = self
            .workspace_change_token
            .strip_prefix(PHASE1_WORKSPACE_METADATA_FINGERPRINT_PREFIX)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Phase 1 workspace change token has an unsupported version",
                )
            })?;
        Ok(current == expected)
    }

    pub async fn validate(
        &self,
        working_dir: &std::path::Path,
        spec: &crate::internal::ai::intentspec::IntentSpec,
    ) -> std::io::Result<()> {
        self.validate_identity(working_dir, spec).await?;
        if !self.workspace_change_matches(working_dir).await? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the workspace changed after this Phase 1 review context was captured",
            ));
        }
        self.validate_identity(working_dir, spec).await?;
        Ok(())
    }
}

impl Phase1ReviewContext {
    pub const SCHEMA_VERSION: u32 = 2;

    pub fn plan_id(&self) -> Option<&str> {
        match &self.persisted_plan {
            Phase1PersistedPlan::Persisted {
                execution_plan_id, ..
            } => Some(execution_plan_id),
            Phase1PersistedPlan::Unavailable => None,
        }
    }
}

/// Provider prompt for the read-only Phase 1 drafting loop.
pub fn phase1_planning_prompt(spec_json: &str) -> String {
    format!(
        "You are generating an execution plan for an already confirmed IntentSpec.\n\
Use read-only repository tools if needed, then call submit_plan_draft exactly once with the full ordered draft.\n\
Every draft step must be a concrete execution task the agent can perform. Provide ordered steps with title only; do not include runtime status.\n\
Do not call submit_intent_draft. Do not modify the IntentSpec. Do not execute commands that change files.\n\
After calling submit_plan_draft, stop; the developer must confirm the compiled plan before execution.\n\n\
Confirmed IntentSpec:\n```json\n{spec_json}\n```"
    )
}

/// Compile provider-proposed Phase 1 steps through the canonical planner.
///
/// The submitted steps replace the confirmed IntentSpec objectives only in
/// the derived planning copy; the durable confirmed IntentSpec remains
/// unchanged.
pub fn compile_submitted_plan(
    spec: &crate::internal::ai::intentspec::IntentSpec,
    draft: &crate::internal::ai::tools::context::SubmitPlanDraftArgs,
) -> Result<crate::internal::ai::orchestrator::types::ExecutionPlanSpec, String> {
    use crate::internal::ai::intentspec::types::{
        ConflictResolution, DecompositionMode, LibraBinding, Objective, ObjectiveKind,
        PlanGenerationConfig,
    };

    crate::internal::ai::tools::context::validate_submit_plan_draft_bounds(draft)?;
    let objective_kind = if spec.intent.has_implementation_objectives() {
        ObjectiveKind::Implementation
    } else {
        ObjectiveKind::Analysis
    };
    let objectives = draft
        .steps
        .iter()
        .filter_map(|step| {
            let title = step.title.trim();
            (!title.is_empty()).then(|| Objective {
                title: title.to_string(),
                kind: objective_kind,
            })
        })
        .collect::<Vec<_>>();
    if objectives.is_empty() || objectives.len() != draft.steps.len() {
        return Err("plan draft must contain only non-empty step titles".to_string());
    }

    let mut planned = spec.clone();
    planned.intent.objectives = objectives;
    let mut libra = planned.libra.take().unwrap_or(LibraBinding {
        object_store: None,
        context_pipeline: None,
        plan_generation: None,
        run_policy: None,
        actor_mapping: None,
        decision_policy: None,
    });
    let generation = libra
        .plan_generation
        .get_or_insert_with(PlanGenerationConfig::default);
    generation.decomposition_mode = DecompositionMode::PerObjective;
    generation.conflict_resolution = ConflictResolution::ForceSerial;
    planned.libra = Some(libra);

    crate::internal::ai::orchestrator::planner::compile_execution_plan_spec(&planned)
        .map_err(|error| error.to_string())
}

pub fn preserve_unchanged_revision_steps(
    revised: &mut crate::internal::ai::orchestrator::types::ExecutionPlanSpec,
    prior: &crate::internal::ai::orchestrator::types::ExecutionPlanSpec,
) {
    fn semantics(
        plan: &crate::internal::ai::orchestrator::types::ExecutionPlanSpec,
        task: &crate::internal::ai::orchestrator::types::TaskSpec,
    ) -> serde_json::Value {
        let dependency_positions = task
            .dependencies()
            .iter()
            .map(|dependency| {
                plan.tasks
                    .iter()
                    .position(|candidate| candidate.id() == *dependency)
                    .map(|position| format!("index:{position}"))
                    .unwrap_or_else(|| format!("missing:{dependency}"))
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "title": task.title(),
            "description": task.description(),
            "dependencies": dependency_positions,
            "constraints": task.constraints(),
            "acceptanceCriteria": task.acceptance_criteria(),
            "objective": task.objective,
            "kind": task.kind,
            "gateStage": task.gate_stage,
            "ownerRole": task.owner_role,
            "scopeIn": task.scope_in,
            "scopeOut": task.scope_out,
            "checks": task.checks,
            "contract": task.contract,
        })
    }

    let revised_semantics = revised
        .tasks
        .iter()
        .map(|task| semantics(revised, task))
        .collect::<Vec<_>>();
    let prior_semantics = prior
        .tasks
        .iter()
        .map(|task| semantics(prior, task))
        .collect::<Vec<_>>();
    let mut used_prior = std::collections::HashSet::new();
    for (task, semantic) in revised.tasks.iter_mut().zip(revised_semantics) {
        let matching =
            prior.tasks.iter().enumerate().find(|(index, _)| {
                !used_prior.contains(index) && prior_semantics[*index] == semantic
            });
        if let Some((index, prior_task)) = matching {
            used_prior.insert(index);
            task.step = prior_task.step.clone();
            task.task.set_origin_step_id(Some(prior_task.step_id()));
        }
    }
}

pub fn phase1_review_context_path(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    interaction_id: &str,
) -> std::path::PathBuf {
    let digest = hex::encode(Sha256::digest(interaction_id.as_bytes()));
    store
        .session_root()
        .join("phase1")
        .join(format!("{digest}.json"))
}

pub fn phase1_start_seed_path(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
) -> std::path::PathBuf {
    store
        .session_root()
        .join("phase1")
        .join("pending-start.json")
}

pub fn phase1_turn_id_from_seed(seed: &Phase1StartSeed) -> std::io::Result<String> {
    if let Some(command_id) = seed.browser_command_id.as_ref() {
        return Ok(command_id.clone());
    }
    let mut identity = Sha256::new();
    identity.update(seed.intent_id.as_bytes());
    identity.update(b"\0");
    identity.update(seed.attempt_id.as_bytes());
    identity.update(b"\0");
    identity.update(
        if seed.source_resolution.eq_ignore_ascii_case("modify") {
            seed.source_interaction_id.as_str()
        } else {
            "confirmed-intent"
        }
        .as_bytes(),
    );
    identity.update(b"\0");
    identity.update(seed.revision_note.as_deref().unwrap_or_default().as_bytes());
    identity.update(b"\0");
    identity.update(serde_json::to_vec(&seed.checkout).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to encode Phase 1 checkout identity: {error}"),
        )
    })?);
    Ok(format!("phase1-web-{}", hex::encode(identity.finalize())))
}

pub fn phase1_formal_write_started_for_seed(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    phase1_turn_id: &str,
    seed_digest: &str,
) -> std::io::Result<bool> {
    let replay = store.load_code_workflow_replay()?;
    let mut matched = false;
    for event in replay.events {
        if let crate::internal::ai::session::CodeWorkflowEventKind::Phase1FormalWriteStarted {
            phase1_turn_id: event_turn_id,
            seed_digest: event_seed_digest,
            ..
        } = event.event
            && event_turn_id == phase1_turn_id
        {
            if event_seed_digest != seed_digest {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Phase 1 turn '{phase1_turn_id}' has a formal-write marker for another start seed"
                    ),
                ));
            }
            matched = true;
        }
    }
    Ok(matched)
}

pub fn persist_phase1_start_seed(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    seed: &Phase1StartSeed,
) -> std::io::Result<()> {
    let body = serde_json::to_vec_pretty(seed).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to serialize Phase 1 start seed: {error}"),
        )
    })?;
    if body.len() > MAX_PHASE1_DURABLE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Phase 1 start seed exceeds the 8 MiB durability limit",
        ));
    }
    crate::utils::atomic_write::write_atomic_with_post_replace_hook(
        &phase1_start_seed_path(store),
        &body,
        true,
        || {
            #[cfg(any(test, feature = "test-provider"))]
            if store.take_phase1_seed_parent_sync_failure_for_test() {
                return Err(std::io::Error::other(
                    "injected failure after Phase 1 start seed replacement and before directory sync",
                ));
            }
            Ok(())
        },
    )
}

/// Persist a Phase 1 start seed without replacing a different in-flight
/// authority. The session writer lease and Web transition serialization make
/// the read/write pair exclusive to the writable headless owner.
pub fn persist_phase1_start_seed_idempotent(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    seed: &Phase1StartSeed,
) -> std::io::Result<()> {
    if let Some(existing) = load_phase1_start_seed(store)? {
        if existing.durable_digest()? == seed.durable_digest()? {
            crate::utils::atomic_write::sync_file_and_parent_durably_with_pre_parent_sync_hook(
                &phase1_start_seed_path(store),
                || {
                    #[cfg(any(test, feature = "test-provider"))]
                    if store.take_phase1_seed_parent_sync_failure_for_test() {
                        return Err(std::io::Error::other(
                            "injected failure while re-syncing the Phase 1 start seed parent directory",
                        ));
                    }
                    Ok(())
                },
            )?;
            return Ok(());
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a different durable Phase 1 start seed is already pending; resume or reconcile it before starting another attempt",
        ));
    }
    persist_phase1_start_seed(store, seed)
}

pub fn load_phase1_start_seed(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
) -> std::io::Result<Option<Phase1StartSeed>> {
    let path = phase1_start_seed_path(store);
    if !path.is_file() {
        return Ok(None);
    }
    let body = read_phase1_bounded(&path, "start seed")?;
    let seed: Phase1StartSeed = serde_json::from_slice(&body).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 start seed at {} is invalid: {error}",
                path.display()
            ),
        )
    })?;
    if seed.schema_version != Phase1StartSeed::SCHEMA_VERSION
        || seed.attempt_id.trim().is_empty()
        || seed.source_interaction_id.trim().is_empty()
        || seed.intent_id.trim().is_empty()
        || seed.intent_spec_id.trim().is_empty()
        || seed.intent_spec_json.trim().is_empty()
        || seed.source_resolution.trim().is_empty()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Phase 1 start seed at {} is incomplete", path.display()),
        ));
    }
    let intent_spec: crate::internal::ai::intentspec::IntentSpec =
        serde_json::from_str(&seed.intent_spec_json).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Phase 1 start seed embeds an invalid IntentSpec: {error}"),
            )
        })?;
    if intent_spec.metadata.id != seed.intent_spec_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 start seed at {} has a mismatched IntentSpec domain id",
                path.display()
            ),
        ));
    }
    seed.checkout.validate_durable_shape()?;
    Ok(Some(seed))
}

pub fn clear_phase1_start_seed(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
) -> std::io::Result<()> {
    crate::utils::atomic_write::remove_durably_with_post_remove_hook(
        &phase1_start_seed_path(store),
        || {
            #[cfg(any(test, feature = "test-provider"))]
            if store.take_phase1_seed_sync_after_remove_failure_for_test() {
                return Err(std::io::Error::other(
                    "injected failure after Phase 1 start seed removal and before directory sync",
                ));
            }
            Ok(())
        },
    )
}

/// Atomically mirror the formal Phase 1 write under the session root before
/// `PlanReviewRequested` is appended.
pub fn persist_phase1_review_context(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    context: &Phase1ReviewContext,
) -> std::io::Result<()> {
    let path = phase1_review_context_path(store, &context.interaction_id);
    if path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "Phase 1 review context at {} is immutable and already exists",
                path.display()
            ),
        ));
    }
    let body = serde_json::to_vec_pretty(context).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to serialize Phase 1 review context: {error}"),
        )
    })?;
    if body.len() > MAX_PHASE1_DURABLE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Phase 1 review context exceeds the 8 MiB durability limit",
        ));
    }
    crate::utils::atomic_write::write_atomic(&path, &body, true)
}

pub fn load_phase1_review_context(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    interaction_id: &str,
) -> std::io::Result<Phase1ReviewContext> {
    let path = phase1_review_context_path(store, interaction_id);
    let body = read_phase1_bounded(&path, "review context")?;
    let context: Phase1ReviewContext = serde_json::from_slice(&body).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 review context at {} is invalid: {error}",
                path.display()
            ),
        )
    })?;
    if context.schema_version != Phase1ReviewContext::SCHEMA_VERSION
        || context.interaction_id != interaction_id
        || context.intent_id.trim().is_empty()
        || context.intent_spec_id != context.intent_spec.metadata.id
        || matches!(
            &context.persisted_plan,
            Phase1PersistedPlan::Persisted {
                execution_plan_id,
                test_plan_id,
            } if execution_plan_id.trim().is_empty() || test_plan_id.trim().is_empty()
        )
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 review context at {} has incompatible identity or schema",
                path.display()
            ),
        ));
    }
    context.checkout.validate_durable_shape()?;
    Ok(context)
}

pub fn validate_phase1_review_context_preflight(
    context: &Phase1ReviewContext,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(context).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to serialize Phase 1 review preflight: {error}"),
        )
    })?;
    if body.len().saturating_add(PHASE1_ID_SERIALIZATION_RESERVE) > MAX_PHASE1_DURABLE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Phase 1 review context would exceed the 8 MiB durability limit",
        ));
    }
    Ok(())
}

pub fn validate_phase1_context_session_budget(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    context: &Phase1ReviewContext,
) -> std::io::Result<()> {
    let new_bytes = serde_json::to_vec(context)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
        .len() as u64
        + PHASE1_ID_SERIALIZATION_RESERVE as u64;
    validate_phase1_context_budget_for_bytes(store, new_bytes)
}

fn validate_phase1_context_budget_for_bytes(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    new_bytes: u64,
) -> std::io::Result<()> {
    // Context sidecars historically live directly under `phase1/`. Restrict
    // the scan to the SHA-256 file shape so pending-start.json and writer
    // leases are never counted, while old sidecars remain budget authority.
    let phase1_root = store.session_root().join("phase1");
    let entries = match std::fs::read_dir(&phase1_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut files = 0usize;
    let mut total = new_bytes;
    for entry in entries {
        let entry = entry?;
        if !is_phase1_review_context_file_name(&entry.file_name()) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Phase 1 review context directory contains a symlink",
            ));
        }
        if file_type.is_file() {
            files = files.saturating_add(1);
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    if files.saturating_add(1) > MAX_PHASE1_CONTEXT_FILES || total > MAX_PHASE1_CONTEXT_TOTAL_BYTES
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "Phase 1 durable context budget is exhausted; finish or cancel older plan reviews before revising again",
        ));
    }
    Ok(())
}

fn is_phase1_review_context_file_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(digest) = name.strip_suffix(".json") else {
        return false;
    };
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn clear_phase1_review_context(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    context_id: &str,
) -> std::io::Result<()> {
    crate::utils::atomic_write::remove_durably(&phase1_review_context_path(store, context_id))
}

/// Remove crash-leftover Phase 1 contexts that are unreachable from the
/// validated workflow authority. A pending start seed may straddle the formal
/// write/marker boundary, so its presence conservatively suppresses GC.
pub fn gc_unreachable_phase1_review_contexts(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
) -> std::io::Result<usize> {
    let replay = store.load_code_workflow_replay_committed()?;
    require_complete_phase1_recovery_replay(&replay)?;
    gc_unreachable_phase1_review_contexts_from_replay(store, &replay)
}

fn gc_unreachable_phase1_review_contexts_from_replay(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
) -> std::io::Result<usize> {
    use std::collections::HashSet;

    if load_phase1_start_seed(store)?.is_some() {
        return Ok(0);
    }
    let events = replay.events.iter().map(|event| &event.event);
    let event_refs = events.collect::<Vec<_>>();
    let mut reachable = HashSet::new();
    for (interaction_id, _, _, _) in open_plan_reviews_from_workflow(event_refs.iter().copied()) {
        if let Some(context_id) =
            phase1_context_id_for_interaction(event_refs.iter().copied(), &interaction_id)
        {
            reachable.insert(context_id);
        }
    }
    for (interaction_id, _, _, _) in open_network_policies_from_workflow(event_refs.iter().copied())
    {
        if let Some(context_id) =
            phase1_context_id_for_gate_interaction(event_refs.iter().copied(), &interaction_id)
        {
            reachable.insert(context_id);
        }
    }
    if let Some(interaction_id) = pending_plan_revision_from_workflow(event_refs.iter().copied())
        && let Some(context_id) =
            phase1_context_id_for_interaction(event_refs.iter().copied(), &interaction_id)
    {
        reachable.insert(context_id);
    }

    let phase1_root = store.session_root().join("phase1");
    let entries = match std::fs::read_dir(&phase1_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry?;
        if !is_phase1_review_context_file_name(&entry.file_name()) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Phase 1 context path '{}' is not a regular file",
                    entry.path().display()
                ),
            ));
        }
        let body = read_phase1_bounded(&entry.path(), "review context")?;
        let context: Phase1ReviewContext = serde_json::from_slice(&body).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Phase 1 review context at {} is invalid: {error}",
                    entry.path().display()
                ),
            )
        })?;
        if phase1_review_context_path(store, &context.interaction_id) != entry.path() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Phase 1 review context at {} has a mismatched immutable identity",
                    entry.path().display()
                ),
            ));
        }
        if !reachable.contains(&context.interaction_id) {
            crate::utils::atomic_write::remove_durably(&entry.path())?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

fn read_phase1_bounded(path: &std::path::Path, label: &str) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_PHASE1_DURABLE_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 {label} at {} exceeds the 8 MiB recovery limit",
                path.display()
            ),
        ));
    }
    let mut body = Vec::new();
    file.take((MAX_PHASE1_DURABLE_BYTES + 1) as u64)
        .read_to_end(&mut body)?;
    if body.len() > MAX_PHASE1_DURABLE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Phase 1 {label} at {} grew beyond the 8 MiB recovery limit",
                path.display()
            ),
        ));
    }
    Ok(body)
}

/// Scan durable Code workflow events for a Plan review gate that was requested
/// but never resolved. Used on session resume so Execute/Modify/Cancel (and the
/// follow-on network-policy gate) cannot disappear across a crash (W2-03).
///
/// Returns the oldest unresolved
/// `(interaction_id, plan_id, turn_id, phase1_turn_id)` tuple. Turn ids may be
/// empty on markers that predate durable gate-turn recovery.
fn open_plan_reviews_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Vec<(String, String, String, String)> {
    use std::collections::{HashMap, HashSet};

    use crate::internal::ai::session::CodeWorkflowEventKind;

    let mut open: HashMap<String, (String, String, String)> = HashMap::new();
    let mut provisional: HashMap<String, (String, String, String, String)> = HashMap::new();
    let mut activated_from_network = HashSet::new();
    let mut order: Vec<String> = Vec::new();
    for event in events {
        match event {
            CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id,
                plan_id,
                turn_id,
                phase1_turn_id,
                prepared_from_network,
                ..
            } => {
                if let Some(network_interaction_id) = prepared_from_network.as_ref() {
                    provisional.insert(
                        network_interaction_id.clone(),
                        (
                            interaction_id.clone(),
                            plan_id.clone(),
                            turn_id.clone(),
                            phase1_turn_id.clone(),
                        ),
                    );
                    continue;
                }
                if open
                    .insert(
                        interaction_id.clone(),
                        (plan_id.clone(), turn_id.clone(), phase1_turn_id.clone()),
                    )
                    .is_none()
                {
                    order.push(interaction_id.clone());
                }
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                ..
            } => {
                if let Some((plan_interaction_id, plan_id, turn_id, phase1_turn_id)) =
                    provisional.remove(interaction_id)
                    && NetworkPolicyDecision::from_wire_id(resolution)
                        == Some(NetworkPolicyDecision::Back)
                    && open
                        .insert(
                            plan_interaction_id.clone(),
                            (plan_id, turn_id, phase1_turn_id),
                        )
                        .is_none()
                {
                    activated_from_network.insert(plan_interaction_id.clone());
                    order.push(plan_interaction_id);
                }
                if resolution.eq_ignore_ascii_case("back-prepare-rollback")
                    && activated_from_network.contains(interaction_id)
                {
                    continue;
                }
                open.remove(interaction_id);
                order.retain(|id| id != interaction_id);
                activated_from_network.remove(interaction_id);
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|interaction_id| {
            open.remove(&interaction_id)
                .map(|(plan_id, turn_id, phase1_turn_id)| {
                    (interaction_id, plan_id, turn_id, phase1_turn_id)
                })
        })
        .collect()
}

pub fn open_plan_review_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Option<(String, String, String, String)> {
    open_plan_reviews_from_workflow(events).into_iter().next()
}

pub fn phase1_context_id_for_interaction<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
    target_interaction_id: &str,
) -> Option<String> {
    let mut found = None;
    for event in events {
        if let crate::internal::ai::session::CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id,
            context_id,
            ..
        } = event
            && interaction_id == target_interaction_id
        {
            found = Some(if context_id.is_empty() {
                interaction_id.clone()
            } else {
                context_id.clone()
            });
        }
    }
    found
}

/// Resolve the immutable Phase 1 context referenced by either a Plan gate or
/// its promoted Network gate. The workflow log remains the authority; callers
/// use this only after a terminal gate resolution is durable to garbage collect
/// the now-unreachable sidecar.
pub fn phase1_context_id_for_gate_interaction<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
    interaction_id: &str,
) -> Option<String> {
    use crate::internal::ai::session::CodeWorkflowEventKind;

    let events = events.into_iter().collect::<Vec<_>>();
    if let Some(context_id) =
        phase1_context_id_for_interaction(events.iter().copied(), interaction_id)
    {
        return Some(context_id);
    }
    let plan_id = events.iter().rev().find_map(|event| match event {
        CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: candidate,
            plan_id,
            ..
        } if candidate == interaction_id => Some(plan_id.as_str()),
        _ => None,
    })?;
    events.iter().rev().find_map(|event| match event {
        CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id,
            plan_id: candidate,
            context_id,
            ..
        } if candidate == plan_id => Some(if context_id.is_empty() {
            interaction_id.clone()
        } else {
            context_id.clone()
        }),
        _ => None,
    })
}

/// Return the latest Plan review whose durable resolution is `modify` and for
/// which no replacement Plan review has subsequently been requested. This is
/// the runtime-owned signal that the next plain Web message is a revision
/// note; adapters do not retain a private pending-plan state.
pub fn pending_plan_revision_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Option<String> {
    use std::collections::{HashMap, HashSet};

    use crate::internal::ai::session::CodeWorkflowEventKind;

    let mut known_reviews = HashSet::new();
    let mut consumed_revision_sources = HashSet::new();
    let mut provisional: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut pending = None;
    for event in events {
        match event {
            CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id,
                revision_of,
                prepared_from_network,
                ..
            } => {
                if let Some(network_interaction_id) = prepared_from_network.as_ref() {
                    provisional.insert(
                        network_interaction_id.clone(),
                        (interaction_id.clone(), revision_of.clone()),
                    );
                    continue;
                }
                known_reviews.insert(interaction_id.clone());
                if let Some(source) = revision_of.as_ref() {
                    consumed_revision_sources.insert(source.clone());
                    if pending.as_ref() == Some(source) {
                        pending = None;
                    }
                } else if pending.as_ref() == Some(interaction_id) {
                    pending = None;
                }
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                ..
            } => {
                if let Some((plan_interaction_id, revision_of)) = provisional.remove(interaction_id)
                    && NetworkPolicyDecision::from_wire_id(resolution)
                        == Some(NetworkPolicyDecision::Back)
                {
                    known_reviews.insert(plan_interaction_id.clone());
                    if let Some(source) = revision_of {
                        consumed_revision_sources.insert(source.clone());
                        if pending.as_ref() == Some(&source) {
                            pending = None;
                        }
                    }
                }
                if !known_reviews.contains(interaction_id)
                    || consumed_revision_sources.contains(interaction_id)
                {
                    continue;
                }
                if PlanReviewDecision::from_wire_id(resolution) == Some(PlanReviewDecision::Revise)
                {
                    pending = Some(interaction_id.clone());
                } else if pending.as_deref() == Some(interaction_id) {
                    pending = None;
                }
            }
            _ => {}
        }
    }
    pending
}

/// Scan durable Code workflow events for a post-plan network-policy gate that
/// was requested but never answered.
///
/// This marker is written *before* the Plan review resolves, so it is the only
/// durable trace of the human network decision once `InteractionResolved`
/// closes the plan gate — without it a crash in that window would silently
/// drop a mandatory human gate on resume (W2-03).
///
/// Returns the oldest unresolved `(interaction_id, plan_id, turn_id,
/// default_allow)` tuple. `turn_id` may be empty on markers that predate
/// durable gate-turn recovery.
fn open_network_policies_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Vec<(String, String, String, bool)> {
    use std::collections::{HashMap, HashSet};

    use crate::internal::ai::session::CodeWorkflowEventKind;

    // Network markers may be written just before or just after Plan Execute
    // settles. Only restore once a durable Execute resolution exists for the
    // matching plan review — never while that review is still open (W2-03 r4).
    // Persisted plans key by `plan_id`; unpersisted plans (empty `plan_id` when
    // MCP/persist fails) associate the marker with the open review interaction
    // so Execute can still promote the mandatory network gate (W2-03 r5).
    let mut open_plan_reviews: HashSet<String> = HashSet::new();
    let mut open_plan_review_order: Vec<String> = Vec::new();
    let mut plan_id_by_review: HashMap<String, String> = HashMap::new();
    let mut open_plan_ids: HashSet<String> = HashSet::new();
    let mut plan_execute_resolved: HashSet<String> = HashSet::new();
    // network interaction_id -> (plan_id, turn_id, default_allow, review_interaction_id)
    let mut pending_network: HashMap<String, (String, String, bool, String)> = HashMap::new();
    let mut open: HashMap<String, (String, String, bool)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut provisional_plan: HashMap<String, (String, String)> = HashMap::new();

    #[allow(clippy::too_many_arguments)]
    fn apply_plan_review_marker(
        interaction_id: &str,
        plan_id: &str,
        open_plan_reviews: &mut HashSet<String>,
        open_plan_review_order: &mut Vec<String>,
        plan_id_by_review: &mut HashMap<String, String>,
        open_plan_ids: &mut HashSet<String>,
        plan_execute_resolved: &mut HashSet<String>,
        pending_network: &mut HashMap<String, (String, String, bool, String)>,
        open: &mut HashMap<String, (String, String, bool)>,
        order: &mut Vec<String>,
    ) {
        if open_plan_reviews.insert(interaction_id.to_string()) {
            open_plan_review_order.push(interaction_id.to_string());
        }
        if !plan_id.is_empty() {
            plan_id_by_review.insert(interaction_id.to_string(), plan_id.to_string());
            open_plan_ids.insert(plan_id.to_string());
            plan_execute_resolved.remove(plan_id);
        }
        plan_execute_resolved.remove(interaction_id);
        let demote: Vec<_> = open
            .iter()
            .filter(|(_, (open_plan_id, _, _))| {
                (!plan_id.is_empty() && open_plan_id == plan_id)
                    || (plan_id.is_empty() && open_plan_id.is_empty())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in demote {
            if let Some((open_plan_id, turn_id, default_allow)) = open.remove(&id) {
                pending_network.insert(
                    id.clone(),
                    (
                        open_plan_id,
                        turn_id,
                        default_allow,
                        interaction_id.to_string(),
                    ),
                );
                order.retain(|open_id| open_id != &id);
            }
        }
    }

    fn promote_ids(
        ready: Vec<String>,
        pending_network: &mut HashMap<String, (String, String, bool, String)>,
        open: &mut HashMap<String, (String, String, bool)>,
        order: &mut Vec<String>,
    ) {
        for id in ready {
            if let Some((plan_id, turn_id, default_allow, _)) = pending_network.remove(&id)
                && open
                    .insert(id.clone(), (plan_id, turn_id, default_allow))
                    .is_none()
            {
                order.push(id);
            }
        }
    }

    fn promote_by_plan_id(
        plan_key: &str,
        pending_network: &mut HashMap<String, (String, String, bool, String)>,
        open: &mut HashMap<String, (String, String, bool)>,
        order: &mut Vec<String>,
    ) {
        if plan_key.is_empty() {
            return;
        }
        let ready: Vec<_> = pending_network
            .iter()
            .filter(|(_, (plan_id, _, _, _))| plan_id == plan_key)
            .map(|(id, _)| id.clone())
            .collect();
        promote_ids(ready, pending_network, open, order);
    }

    fn promote_by_review_id(
        review_id: &str,
        pending_network: &mut HashMap<String, (String, String, bool, String)>,
        open: &mut HashMap<String, (String, String, bool)>,
        order: &mut Vec<String>,
    ) {
        let ready: Vec<_> = pending_network
            .iter()
            .filter(|(_, (plan_id, _, _, associated_review))| {
                associated_review == review_id || (!plan_id.is_empty() && plan_id == review_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        promote_ids(ready, pending_network, open, order);
    }

    for event in events {
        match event {
            CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id,
                plan_id,
                prepared_from_network,
                ..
            } => {
                if let Some(network_interaction_id) = prepared_from_network.as_ref() {
                    provisional_plan.insert(
                        network_interaction_id.clone(),
                        (interaction_id.clone(), plan_id.clone()),
                    );
                    continue;
                }
                apply_plan_review_marker(
                    interaction_id,
                    plan_id,
                    &mut open_plan_reviews,
                    &mut open_plan_review_order,
                    &mut plan_id_by_review,
                    &mut open_plan_ids,
                    &mut plan_execute_resolved,
                    &mut pending_network,
                    &mut open,
                    &mut order,
                );
            }
            CodeWorkflowEventKind::NetworkPolicyRequested {
                interaction_id,
                plan_id,
                turn_id,
                default_allow,
            } => {
                let associated_review = if !plan_id.is_empty() {
                    plan_id_by_review
                        .iter()
                        .find_map(|(review_id, stored_plan)| {
                            (stored_plan == plan_id).then(|| review_id.clone())
                        })
                        .unwrap_or_default()
                } else {
                    // Unpersisted plan: bind to the latest still-open review.
                    open_plan_review_order
                        .iter()
                        .rev()
                        .find(|id| open_plan_reviews.contains(*id))
                        .cloned()
                        .unwrap_or_default()
                };
                pending_network.insert(
                    interaction_id.clone(),
                    (
                        plan_id.clone(),
                        turn_id.clone(),
                        *default_allow,
                        associated_review.clone(),
                    ),
                );
                let plan_approved = (!plan_id.is_empty()
                    && plan_execute_resolved.contains(plan_id))
                    || (!associated_review.is_empty()
                        && plan_execute_resolved.contains(&associated_review))
                    || plan_execute_resolved.contains(interaction_id);
                let plan_still_open = (!plan_id.is_empty() && open_plan_ids.contains(plan_id))
                    || (!associated_review.is_empty()
                        && open_plan_reviews.contains(&associated_review))
                    || open_plan_reviews.contains(interaction_id);
                if plan_approved && !plan_still_open {
                    if !plan_id.is_empty() {
                        promote_by_plan_id(plan_id, &mut pending_network, &mut open, &mut order);
                    } else if !associated_review.is_empty() {
                        promote_by_review_id(
                            &associated_review,
                            &mut pending_network,
                            &mut open,
                            &mut order,
                        );
                    }
                }
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                ..
            } => {
                if let Some((plan_interaction_id, plan_id)) =
                    provisional_plan.remove(interaction_id)
                    && NetworkPolicyDecision::from_wire_id(resolution)
                        == Some(NetworkPolicyDecision::Back)
                {
                    apply_plan_review_marker(
                        &plan_interaction_id,
                        &plan_id,
                        &mut open_plan_reviews,
                        &mut open_plan_review_order,
                        &mut plan_id_by_review,
                        &mut open_plan_ids,
                        &mut plan_execute_resolved,
                        &mut pending_network,
                        &mut open,
                        &mut order,
                    );
                }
                let resolved = resolution.trim().to_ascii_lowercase();
                if resolved == "network-prepare-rollback" && open.contains_key(interaction_id) {
                    // A retry may observe the source Plan command already
                    // terminal after this Network generation was promoted.
                    // Its synthetic prepare rollback must not close the
                    // already-active human gate.
                    continue;
                }
                open_plan_reviews.remove(interaction_id);
                open_plan_review_order.retain(|id| id != interaction_id);
                if let Some(plan_id) = plan_id_by_review.get(interaction_id).cloned() {
                    open_plan_ids.remove(&plan_id);
                }
                open.remove(interaction_id);
                pending_network.remove(interaction_id);
                order.retain(|id| id != interaction_id);
                if matches!(resolved.as_str(), "execute" | "confirm") {
                    plan_execute_resolved.insert(interaction_id.clone());
                    if let Some(plan_id) = plan_id_by_review.get(interaction_id).cloned() {
                        plan_execute_resolved.insert(plan_id.clone());
                        promote_by_plan_id(&plan_id, &mut pending_network, &mut open, &mut order);
                    }
                    // Always promote markers bound to this review interaction
                    // (covers empty `plan_id` after persist/MCP failure).
                    promote_by_review_id(
                        interaction_id,
                        &mut pending_network,
                        &mut open,
                        &mut order,
                    );
                } else {
                    plan_execute_resolved.remove(interaction_id);
                    if let Some(plan_id) = plan_id_by_review.get(interaction_id) {
                        plan_execute_resolved.remove(plan_id);
                    }
                    // Human non-Execute decisions (cancel/modify) drop associated
                    // network markers. Rollback/synthetic resolutions must not —
                    // otherwise a failed Back delivery that revoked a temporary
                    // plan marker would also erase the network gate (W2-03 r20).
                    if matches!(
                        resolved.as_str(),
                        "cancel" | "modify" | "revise" | "network-deny" | "network-allow" | "back"
                    ) {
                        let stale: Vec<_> = pending_network
                            .iter()
                            .filter(|(_, (_, _, _, associated_review))| {
                                associated_review == interaction_id
                            })
                            .map(|(id, _)| id.clone())
                            .collect();
                        for id in stale {
                            pending_network.remove(&id);
                            open.remove(&id);
                            order.retain(|open_id| open_id != &id);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|interaction_id| {
            open.remove(&interaction_id)
                .map(|(plan_id, turn_id, default_allow)| {
                    (interaction_id, plan_id, turn_id, default_allow)
                })
        })
        .collect()
}

pub fn open_network_policy_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Option<(String, String, String, bool)> {
    open_network_policies_from_workflow(events)
        .into_iter()
        .next()
}

/// Fail closed when replay leaves more than one effective human gate.
/// Duplicate markers for the same interaction generation are idempotent; two
/// distinct Intent/Plan/promoted-Network authorities are never safe to choose
/// by ordering because doing so would silently discard a required decision.
pub fn validate_single_open_gate_authority(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
) -> std::io::Result<()> {
    let replay = store.load_code_workflow_replay_committed()?;
    require_complete_phase1_recovery_replay(&replay)?;
    validate_single_open_gate_authority_from_replay(&replay)
}

fn validate_single_open_gate_authority_from_replay(
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
) -> std::io::Result<()> {
    use std::collections::HashSet;

    use crate::internal::ai::session::CodeWorkflowEventKind;

    let mut open_intents = HashSet::new();
    for event in replay.events.iter().map(|event| &event.event) {
        match event {
            CodeWorkflowEventKind::IntentReviewRequested { interaction_id, .. } => {
                open_intents.insert(interaction_id.clone());
            }
            CodeWorkflowEventKind::CommandTerminalFailure {
                command,
                retry_intent_review: Some(retry),
                ..
            } => {
                validate_phase1_retry_intent_review_shape(retry, &command.command_id)?;
                open_intents.insert(retry.interaction_id.clone());
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                prior_interaction_resolutions,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                prior_interaction_resolutions,
                ..
            } => {
                for resolved_id in prior_interaction_resolutions
                    .iter()
                    .map(|(interaction_id, _)| interaction_id)
                    .chain(std::iter::once(interaction_id))
                {
                    open_intents.remove(resolved_id);
                }
            }
            _ => {}
        }
    }
    let open_plans =
        open_plan_reviews_from_workflow(replay.events.iter().map(|event| &event.event));
    let open_networks =
        open_network_policies_from_workflow(replay.events.iter().map(|event| &event.event));
    let total = open_intents
        .len()
        .saturating_add(open_plans.len())
        .saturating_add(open_networks.len());
    if total > 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Code workflow has conflicting open gate authority (intent={}, plan={}, network={}); session requires reconciliation",
                open_intents.len(),
                open_plans.len(),
                open_networks.len()
            ),
        ));
    }
    Ok(())
}

fn require_complete_phase1_recovery_replay(
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
) -> std::io::Result<()> {
    if crate::internal::ai::session::jsonl::intent_revision_replay_is_complete(replay) {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "Phase 1 recovery requires a complete Code workflow replay (sequence_gaps={}, window_cut_mid_record={}); no gate authority or context garbage collection was applied",
            replay.gaps.len(),
            replay.window_cut_mid_record
        ),
    ))
}

/// Establish a committed workflow-log view before startup makes any gate or
/// context-GC decision. A complete JSONL row can be visible after its writer
/// reported a sync failure; syncing first prevents that volatile row from
/// authorizing an irreversible sidecar deletion.
pub fn prepare_phase1_recovery_authority(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
) -> std::io::Result<usize> {
    let replay = store.load_code_workflow_replay_committed()?;
    require_complete_phase1_recovery_replay(&replay)?;
    validate_single_open_gate_authority_from_replay(&replay)?;
    gc_unreachable_phase1_review_contexts_from_replay(store, &replay)
}

/// Resolve the single mutating command id that startup recovery is allowed to
/// complete as success because a human review gate still owns its result.
///
/// Phase 0 wins when an IntentSpec review is open (the plan draft cannot exist
/// yet); otherwise an open Plan review contributes its Phase 1 turn. Every
/// other pending mutation stays fenced, so this never widens recovery beyond
/// the one draft-writing turn whose durable marker proves it finished.
///
/// Returns `None` when no review gate is open, when the marker predates
/// turn-id recovery (empty id), or when the workflow log cannot be read — all
/// of which keep the caller on the strict "fence every pending mutation" path.
pub fn open_review_gate_phase_turn_id(
    store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
) -> Option<String> {
    let replay = store.load_code_workflow_replay().ok()?;
    let phase0_turn_id =
        super::phase0::open_intent_review_from_workflow(replay.events.iter().map(|e| &e.event))
            .map(|(_, _, _, phase0_turn_id)| phase0_turn_id)
            .filter(|id| !id.is_empty());
    if phase0_turn_id.is_some() {
        return phase0_turn_id;
    }
    let open_plan = open_plan_review_from_workflow(replay.events.iter().map(|e| &e.event))
        .map(|(_, _, _, phase1_turn_id)| phase1_turn_id)
        .filter(|id| !id.is_empty());
    if open_plan.is_some() {
        return open_plan;
    }

    // Executing a Plan resolves its review marker before opening the
    // non-mutating Network gate. The Network marker still proves that the
    // corresponding Phase 1 formal write completed, so recover the same
    // mutating command as success instead of fencing it after a crash.
    let (_, network_plan_id, _, _) =
        open_network_policy_from_workflow(replay.events.iter().map(|e| &e.event))?;
    replay
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.event {
            crate::internal::ai::session::CodeWorkflowEventKind::PlanReviewRequested {
                plan_id,
                phase1_turn_id,
                ..
            } if plan_id == &network_plan_id && !phase1_turn_id.is_empty() => {
                Some(phase1_turn_id.clone())
            }
            _ => None,
        })
}

/// Phase 1 planning tool-loop policy shared by every caller that drives the
/// execution-plan drafting conversation. `submit_plan_draft` is the only
/// terminal tool so Phase 1 cannot silently fall through into a mutating tool
/// before the formal write in [`write_plan_set`].
pub fn phase1_plan_tool_loop_config(mut config: ToolLoopConfig) -> ToolLoopConfig {
    config.allowed_tools = Some(vec![
        "read_file".to_string(),
        "list_dir".to_string(),
        "grep_files".to_string(),
        "search_files".to_string(),
        "web_search".to_string(),
        "submit_plan_draft".to_string(),
    ]);
    config.terminal_tools = Some(vec!["submit_plan_draft".to_string()]);
    config.max_turns = Some(12);
    config
}

/// Developer's decision on a pending Plan review
/// ([`super::worker::InteractionState::AwaitingPlanReview`]).
///
/// Stable wire ids match the existing post-plan Code UI options
/// (`execute` / `modify` / `cancel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanReviewDecision {
    /// Accept the plan draft and proceed to the network-policy human gate.
    Execute,
    /// Reject the draft and wait for a plain-text plan revision.
    Revise,
    /// Abandon the review; no execution is started.
    Cancel,
}

impl PlanReviewDecision {
    pub fn wire_id(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Revise => "modify",
            Self::Cancel => "cancel",
        }
    }

    pub fn from_wire_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "execute" | "confirm" => Some(Self::Execute),
            "modify" | "revise" => Some(Self::Revise),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    /// Map the retired TUI's `PendingPostPlan::selected` index
    /// (0=Execute, 1=Modify, 2=Cancel) for legacy persisted state.
    pub fn from_choice_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Execute),
            1 => Some(Self::Revise),
            2 => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// Developer's decision on the post-plan network-policy gate
/// ([`super::worker::InteractionState::AwaitingNetworkPolicy`]).
///
/// `--network-access allow` still requires an explicit human choice here;
/// Web-only defaults must not skip this gate (W2-03 AC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkPolicyDecision {
    Deny,
    Allow,
    /// Return to the Plan review dialog without releasing execution.
    Back,
}

impl NetworkPolicyDecision {
    pub fn wire_id(self) -> &'static str {
        match self {
            Self::Deny => "network-deny",
            Self::Allow => "network-allow",
            Self::Back => "back",
        }
    }

    pub fn from_wire_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "network-deny" | "deny" => Some(Self::Deny),
            "network-allow" | "allow" => Some(Self::Allow),
            "back" => Some(Self::Back),
            _ => None,
        }
    }

    /// Map the retired TUI's `PendingNetworkPolicyChoice::selected` index
    /// (0=Deny, 1=Allow, 2=Back) for legacy persisted state.
    pub fn from_choice_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Deny),
            1 => Some(Self::Allow),
            2 => Some(Self::Back),
            _ => None,
        }
    }

    pub fn network_access(self) -> Option<bool> {
        match self {
            Self::Deny => Some(false),
            Self::Allow => Some(true),
            Self::Back => None,
        }
    }
}

/// Stable interaction id for the network-policy gate that follows Plan Execute.
pub fn network_policy_interaction_id(plan_id: Option<&str>) -> String {
    match plan_id {
        Some(plan_id) => format!("{plan_id}:network-policy"),
        None => "post-plan-network-policy".to_string(),
    }
}

/// [`RuntimeInteractionDelivery`] for the Phase 1 Plan review gate.
///
/// `Execute` parks with [`RuntimeTurnExecution::CompletedHoldQueued`] so the
/// network-policy gate can register next without admitting mutating work.
/// `Revise` / `Cancel` use [`RuntimeTurnExecution::CompletedDiscardQueued`].
#[derive(Debug, Clone, Default)]
pub struct PlanReviewAckDelivery;

impl PlanReviewAckDelivery {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeInteractionDelivery for PlanReviewAckDelivery {
    fn validate(&self, interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
        PlanReviewDecision::from_wire_id(&interaction.response)
            .map(|_| ())
            .ok_or_else(|| {
                RuntimeWorkerError::InvalidInteractionResponse(format!(
                    "unrecognized Plan review response '{}'; expected one of execute/modify/cancel",
                    interaction.response
                ))
            })
    }

    fn persist_interaction_resolved_after_terminal(&self) -> bool {
        true
    }

    fn interaction_resolution(&self, interaction: &InteractionResponse) -> String {
        PlanReviewDecision::from_wire_id(&interaction.response)
            .map(|decision| decision.wire_id().to_string())
            .unwrap_or_else(|| interaction.response.clone())
    }

    fn preserve_pending_on_shutdown(&self) -> bool {
        true
    }

    async fn deliver(
        self: Box<Self>,
        _request: TurnRequest,
        interaction: InteractionResponse,
        _context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        let decision =
            PlanReviewDecision::from_wire_id(&interaction.response).ok_or_else(|| {
                RuntimeWorkerError::ExecutionFailed(format!(
                    "unrecognized Plan review response '{}'",
                    interaction.response
                ))
            })?;
        match decision {
            PlanReviewDecision::Execute => Ok(RuntimeTurnExecution::CompletedHoldQueued {
                summary: "Plan confirmed; awaiting network policy choice".to_string(),
            }),
            PlanReviewDecision::Revise => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary: "Plan revision requested".to_string(),
            }),
            PlanReviewDecision::Cancel => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary: "Plan review cancelled".to_string(),
            }),
        }
    }
}

/// [`RuntimeInteractionDelivery`] for the post-plan network-policy gate.
///
/// `Allow` / `Deny` complete the gate turn (execution hand-off is a caller /
/// W2-04 concern). `Back` discards any work queued under the network fence and
/// returns control so Plan review can be re-opened.
#[derive(Debug, Clone, Default)]
pub struct NetworkPolicyAckDelivery;

impl NetworkPolicyAckDelivery {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeInteractionDelivery for NetworkPolicyAckDelivery {
    fn validate(&self, interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
        NetworkPolicyDecision::from_wire_id(&interaction.response)
            .map(|_| ())
            .ok_or_else(|| {
                RuntimeWorkerError::InvalidInteractionResponse(format!(
                    "unrecognized network policy response '{}'; expected one of network-deny/network-allow/back",
                    interaction.response
                ))
            })
    }

    fn persist_interaction_resolved_after_terminal(&self) -> bool {
        true
    }

    fn interaction_resolution(&self, interaction: &InteractionResponse) -> String {
        NetworkPolicyDecision::from_wire_id(&interaction.response)
            .map(|decision| decision.wire_id().to_string())
            .unwrap_or_else(|| interaction.response.clone())
    }

    fn preserve_pending_on_shutdown(&self) -> bool {
        true
    }

    async fn deliver(
        self: Box<Self>,
        _request: TurnRequest,
        interaction: InteractionResponse,
        _context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        let decision =
            NetworkPolicyDecision::from_wire_id(&interaction.response).ok_or_else(|| {
                RuntimeWorkerError::ExecutionFailed(format!(
                    "unrecognized network policy response '{}'",
                    interaction.response
                ))
            })?;
        match decision {
            NetworkPolicyDecision::Deny => Ok(RuntimeTurnExecution::Completed {
                summary: "Network policy: deny; ready to hand off plan execution".to_string(),
            }),
            NetworkPolicyDecision::Allow => Ok(RuntimeTurnExecution::Completed {
                summary: "Network policy: allow; ready to hand off plan execution".to_string(),
            }),
            NetworkPolicyDecision::Back => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary: "Returned to plan review from network policy".to_string(),
            }),
        }
    }
}

/// Outcome of the [`write_plan_set`] entry point: identifiers for
/// the paired execution / test plan revisions and the
/// `task_id → plan_id` map the scheduler will use to advance.
///
/// **Stability contract:** field names are part of the public Runtime
/// surface once `write_plan_set` ships; downstream observers / audit code
/// will key off `execution_plan_id` and `test_plan_id`. New fields may be
/// added as `Option<...>` or `#[serde(default)]`; existing fields cannot be
/// renamed or removed without a parallel deprecation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanWriteOutcome {
    /// Identifier of the persisted execution-plan revision.
    pub execution_plan_id: String,
    /// Identifier of the paired test-plan revision (Libra always creates
    /// execution + test plans together so Phase 3 validation has a stable
    /// reference).
    pub test_plan_id: String,
    /// Map from logical `task_id` (UUID assigned at intent canonicalisation
    /// time) to the persisted `plan_id` that owns the corresponding step.
    /// The Scheduler reads this to thread `task_id` ↔ `plan_id` for `dagrs`
    /// node addressing and for the `agent_usage_stats.plan_id` column.
    pub plan_id_by_task_id: std::collections::HashMap<uuid::Uuid, String>,
}

/// Errors returned by [`apply_scheduler_mutation`] when the input state
/// or mutation can't be advanced.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApplySchedulerMutationError {
    /// The mutation's expected `scheduler` version doesn't match the
    /// state's current version. Caller should reload state and retry.
    #[error("scheduler version mismatch: mutation expected {expected}, state at {actual}")]
    VersionMismatch { expected: i64, actual: i64 },
    /// `SeedThread` was applied to a state whose `thread_id` doesn't
    /// match the seed bundle's `thread_id`. Cross-thread seeding would
    /// silently corrupt projection state, so the helper fails-closed
    /// and forces the caller to load the correct state first.
    #[error(
        "SeedThread bundle thread_id {bundle_thread_id} does not match scheduler state \
         thread_id {state_thread_id}; seeding cross-thread is not allowed"
    )]
    SeedThreadMismatch {
        bundle_thread_id: uuid::Uuid,
        state_thread_id: uuid::Uuid,
    },
    /// The mutation variant doesn't yet have a wired implementation in
    /// this helper. Wave 1B follow-up will fold the orchestrator's
    /// existing scheduler updates into this function; until then,
    /// unsupported variants surface this error so callers can route
    /// through the legacy `orchestrator::persistence` path.
    #[error(
        "scheduler mutation variant {variant} is not yet wired by apply_scheduler_mutation; \
         route through orchestrator::persistence for now"
    )]
    VariantNotWired { variant: &'static str },
}

/// Apply a [`SchedulerMutation`](crate::internal::ai::runtime::contracts::SchedulerMutation)
/// to a [`SchedulerState`](crate::internal::ai::projection::scheduler::SchedulerState)
/// snapshot, returning the next state.
///
/// **Pure function** — no DB IO; the caller is responsible for loading
/// `current` via
/// [`SchedulerStateRepository::load`](crate::internal::ai::projection::scheduler::SchedulerStateRepository::load)
/// and persisting the returned state via
/// [`SchedulerStateRepository::compare_and_swap`](crate::internal::ai::projection::scheduler::SchedulerStateRepository::compare_and_swap).
///
/// # Wired variants (all 8 SchedulerMutation kinds, v0.17.590)
///
/// - `SeedThread { bundle }` (v0.17.590) — initializes a fresh thread:
///   clears active task / run / plan heads, records the seed bundle
///   (`intent_id` + optional `context_snapshot_id`) under
///   `metadata.seed_bundle`, and removes any prior `stale_reason` /
///   `stage` markers. Fails-closed with `SeedThreadMismatch` when the
///   bundle's `thread_id` doesn't match the state's.
/// - `SetCurrentPlanHeads { execution_plan_id, test_plan_id }`
///   (v0.17.589) — sets `current_plan_heads` to `[execution(ordinal 0),
///   test(ordinal 1)]`; mirrors `selected_plan_id` to the execution
///   head.
/// - `SelectPlanSet { selected }` (v0.17.589) — populates
///   `selected_plan_ids` from `SelectedPlanSet::ordered_ids()`;
///   mirrors `selected_plan_id` to the execution head.
/// - `StartStage { stage }` (v0.17.589) — writes a stable
///   lower-snake-case `stage` ("execution" / "test") into `metadata`
///   and clears any prior `stale_reason` marker.
/// - `MarkTaskActive { task_id, run_id }` (v0.17.588) — sets
///   `active_task_id = Some(task_id)` and `active_run_id = run_id`.
/// - `ClearActiveRun { .. }` (v0.17.588) — clears `active_run_id` to
///   `None` while preserving `active_task_id`.
/// - `MarkProjectionStale { reason }` (v0.17.589) — persists the
///   reason as a stable lower-snake-case `stale_reason` key in
///   metadata; future `ApplyRebuild` removes it.
/// - `ApplyRebuild { materialized }` (v0.17.589) — clears
///   `metadata.stale_reason` and records `metadata.rebuild_versions`
///   (`{thread, scheduler, live_context_window}`) so observers can
///   correlate rebuild events with their version triple.
///
/// All variants bump `version` by 1 and refresh `updated_at`.
///
/// # Errors
///
/// - [`ApplySchedulerMutationError::VersionMismatch`] when the
///   mutation's `expected.scheduler` doesn't match `current.version`.
///   The caller should reload state and retry.
/// - [`ApplySchedulerMutationError::SeedThreadMismatch`] only on
///   `SeedThread` when the bundle's `thread_id` differs from the
///   state's `thread_id` — fail-closed to prevent cross-thread
///   seeding.
/// - [`ApplySchedulerMutationError::VariantNotWired`] retained for
///   forward compatibility (future `SchedulerMutation` variants land
///   here first as `VariantNotWired` before being wired); currently
///   unreachable on the 8 existing variants.
pub fn apply_scheduler_mutation(
    current: &crate::internal::ai::projection::scheduler::SchedulerState,
    mutation: crate::internal::ai::runtime::contracts::SchedulerMutation,
) -> Result<crate::internal::ai::projection::scheduler::SchedulerState, ApplySchedulerMutationError>
{
    use crate::internal::ai::runtime::contracts::SchedulerMutation;

    let expected = mutation.expected_versions().scheduler;
    if current.version != expected {
        return Err(ApplySchedulerMutationError::VersionMismatch {
            expected,
            actual: current.version,
        });
    }

    let mut next = current.clone();
    next.version = current.version + 1;
    next.updated_at = chrono::Utc::now();

    use serde_json::json;

    use crate::internal::ai::projection::scheduler::PlanHeadRef;

    match mutation {
        SchedulerMutation::MarkTaskActive {
            task_id, run_id, ..
        } => {
            next.active_task_id = Some(task_id);
            next.active_run_id = run_id;
        }
        SchedulerMutation::ClearActiveRun { .. } => {
            next.active_run_id = None;
        }
        SchedulerMutation::SetCurrentPlanHeads {
            execution_plan_id,
            test_plan_id,
            ..
        } => {
            // Execution plan is ordinal 0 (primary), test plan is ordinal
            // 1. `selected_plan_id` keeps the legacy single-plan field
            // pointing at the execution head so older readers don't break.
            next.current_plan_heads = vec![
                PlanHeadRef {
                    plan_id: execution_plan_id,
                    ordinal: 0,
                },
                PlanHeadRef {
                    plan_id: test_plan_id,
                    ordinal: 1,
                },
            ];
            next.selected_plan_id = Some(execution_plan_id);
        }
        SchedulerMutation::SelectPlanSet { selected, .. } => {
            let ordered = selected.ordered_ids();
            next.selected_plan_ids = ordered
                .iter()
                .enumerate()
                .map(|(ordinal, plan_id)| PlanHeadRef {
                    plan_id: *plan_id,
                    ordinal: ordinal as i64,
                })
                .collect();
            // Keep `selected_plan_id` in sync with the execution head
            // (the first ordered id, per `SelectedPlanSet::ordered_ids`).
            next.selected_plan_id = Some(selected.execution_plan_id);
        }
        SchedulerMutation::StartStage { stage, .. } => {
            // The stage is scheduler metadata, not a structural field.
            // Merge it into `metadata` under a stable "stage" key so
            // downstream readers (Web Code UI, MCP observability) can pick it up
            // without needing to introduce a new SchedulerState column.
            let stage_label = match stage {
                crate::internal::ai::runtime::contracts::DagStage::Execution => "execution",
                crate::internal::ai::runtime::contracts::DagStage::Test => "test",
            };
            let mut metadata = next.metadata.clone().unwrap_or_else(|| json!({}));
            if let Some(obj) = metadata.as_object_mut() {
                obj.insert("stage".to_string(), json!(stage_label));
                obj.remove("stale_reason");
            }
            next.metadata = Some(metadata);
        }
        SchedulerMutation::MarkProjectionStale { reason, .. } => {
            // Mark the projection as stale by writing the reason into
            // `metadata.stale_reason`. The next `ApplyRebuild` will
            // remove this key; ad-hoc readers SHOULD treat the presence
            // of `stale_reason` as "consult ProjectionResolver before
            // trusting this state".
            let reason_label = match reason {
                crate::internal::ai::runtime::contracts::ProjectionStaleReason::RebuildRequired => {
                    "rebuild_required"
                }
                crate::internal::ai::runtime::contracts::ProjectionStaleReason::DerivedRecordStale => {
                    "derived_record_stale"
                }
                crate::internal::ai::runtime::contracts::ProjectionStaleReason::CasConflict => {
                    "cas_conflict"
                }
                crate::internal::ai::runtime::contracts::ProjectionStaleReason::Backpressure => {
                    "backpressure"
                }
                crate::internal::ai::runtime::contracts::ProjectionStaleReason::ManualRepair => {
                    "manual_repair"
                }
            };
            let mut metadata = next.metadata.clone().unwrap_or_else(|| json!({}));
            if let Some(obj) = metadata.as_object_mut() {
                obj.insert("stale_reason".to_string(), json!(reason_label));
            }
            next.metadata = Some(metadata);
        }
        SchedulerMutation::ApplyRebuild { materialized, .. } => {
            // A rebuild replaces the projection with a freshly
            // materialized snapshot. The `materialized.summary` field
            // is intentionally an opaque `serde_json::Value` here (its
            // exact shape is owned by `ProjectionResolver`); we adopt
            // its versions and clear the `stale_reason` marker so
            // subsequent readers know the rebuild has landed. Caller-
            // managed structural fields (`active_task_id`,
            // `active_run_id`, plan heads, etc.) are left to the
            // caller — `ApplyRebuild` is about projection freshness,
            // not about per-task scheduling.
            let mut metadata = next.metadata.clone().unwrap_or_else(|| json!({}));
            if let Some(obj) = metadata.as_object_mut() {
                obj.remove("stale_reason");
                obj.insert(
                    "rebuild_versions".to_string(),
                    json!({
                        "thread": materialized.versions.thread,
                        "scheduler": materialized.versions.scheduler,
                        "live_context_window": materialized.versions.live_context_window,
                    }),
                );
            }
            next.metadata = Some(metadata);
        }
        SchedulerMutation::SeedThread { bundle, .. } => {
            // SeedThread is the per-thread initialization step: a fresh
            // SchedulerState has no active task / run, no plan heads,
            // no selected plan set — the subsequent mutations
            // (SelectPlanSet → SetCurrentPlanHeads → MarkTaskActive)
            // fill those in.
            //
            // We do require the seed bundle's `thread_id` to match the
            // state's `thread_id` — seeding a state for a different
            // thread would be a cross-thread write and should fail-
            // closed.
            if bundle.thread_id != current.thread_id {
                return Err(ApplySchedulerMutationError::SeedThreadMismatch {
                    bundle_thread_id: bundle.thread_id,
                    state_thread_id: current.thread_id,
                });
            }
            next.active_task_id = None;
            next.active_run_id = None;
            next.selected_plan_id = None;
            next.selected_plan_ids = Vec::new();
            next.current_plan_heads = Vec::new();
            // Record the seed bundle in metadata so observers can
            // correlate the seed event with the originating Intent /
            // ContextSnapshot identifiers without re-reading the
            // append-only event log.
            let mut metadata = next.metadata.clone().unwrap_or_else(|| json!({}));
            if let Some(obj) = metadata.as_object_mut() {
                obj.insert(
                    "seed_bundle".to_string(),
                    json!({
                        "intent_id": bundle.intent_id,
                        "context_snapshot_id": bundle.context_snapshot_id,
                    }),
                );
                // Seeding a fresh thread invalidates any prior
                // freshness signals.
                obj.remove("stale_reason");
                obj.remove("stage");
            }
            next.metadata = Some(metadata);
        }
    }

    Ok(next)
}

/// Errors returned by [`advance_scheduler`] when the async load → apply →
/// CAS-save cycle can't complete.
#[derive(Debug, thiserror::Error)]
pub enum AdvanceSchedulerError {
    /// Scheduler state for the target thread doesn't exist. Caller
    /// should `SeedThread` the projection first.
    #[error("scheduler state for thread {thread_id} does not exist")]
    StateMissing { thread_id: uuid::Uuid },
    /// The pure-function apply step rejected the mutation. See
    /// [`ApplySchedulerMutationError`] for the specific reason.
    #[error(transparent)]
    Apply(#[from] ApplySchedulerMutationError),
    /// The CAS save failed — either a concurrent writer raced us or
    /// the underlying storage returned an error. See
    /// [`crate::internal::ai::projection::scheduler::SchedulerStateCasError`]
    /// for the specific cause.
    #[error(transparent)]
    Cas(#[from] crate::internal::ai::projection::scheduler::SchedulerStateCasError),
    /// The repository load itself failed (DB error, deserialization,
    /// etc.). Distinct from `StateMissing` which is the load-OK-but-no-
    /// row case.
    #[error("scheduler state load failed: {0}")]
    Load(String),
}

/// Async wrapper around [`apply_scheduler_mutation`]: loads the current
/// scheduler state for `thread_id` from the repository, applies the
/// mutation in-memory, then CAS-saves the result.
///
/// This is the **formal-write entry point** for Phase 1 scheduler
/// advances; callers should prefer it over driving
/// [`SchedulerStateRepository`](crate::internal::ai::projection::scheduler::SchedulerStateRepository)
/// directly because:
///
/// 1. It enforces the version-equality precondition (the pure
///    `apply_scheduler_mutation` checks `mutation.expected.scheduler ==
///    current.version` before applying) so CAS conflicts surface as
///    `ApplySchedulerMutationError::VersionMismatch` instead of as a
///    raw CAS error.
/// 2. It centralises the load-then-CAS pattern so future variants
///    (e.g. retry-on-conflict, observer hooks) only need to land in
///    one place.
///
/// # Errors
///
/// Returns [`AdvanceSchedulerError`] which transparently re-exports the
/// apply-side ([`ApplySchedulerMutationError`]) and CAS-side
/// ([`crate::internal::ai::projection::scheduler::SchedulerStateCasError`])
/// errors so callers can route on either kind.
pub async fn advance_scheduler(
    repo: &crate::internal::ai::projection::scheduler::SchedulerStateRepository,
    thread_id: uuid::Uuid,
    mutation: crate::internal::ai::runtime::contracts::SchedulerMutation,
) -> Result<crate::internal::ai::projection::scheduler::SchedulerState, AdvanceSchedulerError> {
    let current = repo
        .load(thread_id)
        .await
        .map_err(|err| AdvanceSchedulerError::Load(err.to_string()))?
        .ok_or(AdvanceSchedulerError::StateMissing { thread_id })?;

    let expected_version = current.version;
    let next = apply_scheduler_mutation(&current, mutation)?;

    repo.compare_and_swap(expected_version, &next).await?;

    Ok(next)
}

/// Persist a new plan set as the **formal write** for Phase 1.
///
/// Bridges into
/// [`crate::internal::ai::orchestrator::persistence::write_plan_set_with_outcome`]
/// so the orchestrator's existing `PersistedPlanRevision` /
/// `step_id_map` plumbing stays where it lives today, while the public
/// contract surface (this function + [`PlanWriteOutcome`]) is owned by
/// the Runtime. Once the orchestrator's persistence layer is folded into
/// this module, the bridge disappears.
///
/// # Errors
///
/// Returns the underlying
/// [`crate::internal::ai::orchestrator::types::OrchestratorError`]
/// unchanged so callers can route on the existing error variants without
/// a new typed-error wrapper.
pub async fn write_plan_set(
    mcp_server: &std::sync::Arc<crate::internal::ai::mcp::server::LibraMcpServer>,
    intent_id: &str,
    parent_execution_plan_id: Option<&str>,
    parent_test_plan_id: Option<&str>,
    plan: &crate::internal::ai::orchestrator::types::ExecutionPlanSpec,
) -> Result<PlanWriteOutcome, crate::internal::ai::orchestrator::types::OrchestratorError> {
    crate::internal::ai::orchestrator::persistence::write_plan_set_with_outcome(
        mcp_server,
        intent_id,
        parent_execution_plan_id,
        parent_test_plan_id,
        plan,
    )
    .await
}

impl PlanWriteOutcome {
    /// Returns the (execution, test) plan id pair as the canonical
    /// scheduler-facing ordering.
    ///
    /// `SchedulerMutation::SetCurrentPlanHeads` expects the execution head
    /// before the test head, matching
    /// [`crate::internal::ai::runtime::contracts::SelectedPlanSet::ordered_ids`].
    pub fn ordered_plan_ids(&self) -> (&str, &str) {
        (self.execution_plan_id.as_str(), self.test_plan_id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use git_internal::internal::object::{plan::PlanStep, task::Task as GitTask, types::ActorRef};
    use uuid::Uuid;

    use super::*;
    use crate::{
        internal::{
            ai::{
                history::HistoryManager,
                intentspec::{
                    ResolveContext,
                    draft::{DraftAcceptance, DraftIntent, DraftRisk, IntentDraft},
                    resolve_intentspec,
                    types::{ChangeType, Objective, ObjectiveKind, RiskLevel},
                },
                mcp::{resource::CreateIntentParams, server::LibraMcpServer},
                orchestrator::types::{
                    ExecutionPlanSpec, GateStage, TaskContract, TaskKind, TaskSpec,
                },
            },
            db,
        },
        utils::{storage::local::LocalStorage, test},
    };

    #[test]
    fn phase1_review_context_path_hashes_untrusted_interaction_id() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp_dir.path().to_path_buf(),
        );
        let path = phase1_review_context_path(&store, "../../outside/review");
        let expected_parent = temp_dir.path().join("phase1");
        assert_eq!(path.parent(), Some(expected_parent.as_path()));
        let file_name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("hashed file name");
        assert_eq!(file_name.len(), 64 + ".json".len());
        assert!(file_name.ends_with(".json"));
        assert!(
            file_name
                .trim_end_matches(".json")
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert!(!path.to_string_lossy().contains("outside"));
    }

    #[test]
    fn phase1_checkout_binding_accepts_unborn_branch_but_not_detached_without_oid() {
        let mut binding = Phase1CheckoutBinding {
            canonical_working_dir: "/repo/main".to_string(),
            repo_id: "repo-id".to_string(),
            repo_locator: "/repo/main".to_string(),
            base_ref: "HEAD".to_string(),
            workspace_fingerprint: "0".repeat(64),
            workspace_change_token: String::new(),
            head_oid: None,
            branch_label: "main".to_string(),
            worktree_id: None,
        };

        binding
            .validate_durable_shape()
            .expect("an unborn named branch is a valid checkout binding");

        binding.workspace_change_token = format!(
            "{PHASE1_WORKSPACE_METADATA_FINGERPRINT_PREFIX}{}",
            "1".repeat(64)
        );
        binding
            .validate_durable_shape()
            .expect("the additive metadata change token is valid");

        binding.workspace_fingerprint = binding.workspace_change_token.clone();
        assert!(
            binding.validate_durable_shape().is_err(),
            "a metadata-only token must never replace the content authority"
        );
        binding.workspace_fingerprint = "0".repeat(64);

        let mut moved_head = binding.clone();
        moved_head.head_oid = Some("1".repeat(40));
        assert!(binding.same_intent_repository_as(&moved_head));
        let mut replaced_repo = binding.clone();
        replaced_repo.repo_id = "replacement-repo".to_string();
        assert!(!binding.same_intent_repository_as(&replaced_repo));

        binding.branch_label = format!("detached:{}", "0".repeat(40));
        assert!(
            binding.validate_durable_shape().is_err(),
            "a detached checkout must retain its exact object id"
        );
    }

    #[tokio::test]
    async fn phase1_workspace_binding_uses_content_authority_and_legacy_fallback() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("create workspace");
        let path = root.join("README.md");
        std::fs::write(&path, "before\n").expect("write baseline");
        let metadata = std::fs::metadata(&path).expect("baseline metadata");
        let modified = metadata.modified().expect("baseline mtime");
        let accessed = metadata.accessed().expect("baseline atime");
        let canonical = std::fs::canonicalize(&root).expect("canonical workspace");
        let content =
            crate::internal::ai::workspace_snapshot::workspace_snapshot_fingerprint(&canonical)
                .expect("content fingerprint");
        let change_token =
            crate::internal::ai::workspace_snapshot::workspace_snapshot_metadata_fingerprint(
                &canonical,
            )
            .expect("metadata change token");
        let binding = Phase1CheckoutBinding {
            canonical_working_dir: canonical.to_string_lossy().into_owned(),
            repo_id: "repo-id".to_string(),
            repo_locator: canonical.to_string_lossy().into_owned(),
            base_ref: "HEAD".to_string(),
            workspace_fingerprint: content,
            workspace_change_token: format!(
                "{PHASE1_WORKSPACE_METADATA_FINGERPRINT_PREFIX}{change_token}"
            ),
            head_oid: Some("0".repeat(40)),
            branch_label: "main".to_string(),
            worktree_id: None,
        };
        let mut legacy_json = serde_json::to_value(&binding).expect("serialize binding");
        legacy_json
            .as_object_mut()
            .expect("binding JSON object")
            .remove("workspaceChangeToken");
        let legacy: Phase1CheckoutBinding =
            serde_json::from_value(legacy_json).expect("decode legacy content-only binding");
        assert!(legacy.workspace_change_token.is_empty());
        assert!(
            binding
                .workspace_matches(&canonical)
                .await
                .expect("baseline content comparison")
        );

        std::fs::write(&path, "after!\n").expect("same-length rewrite");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open rewritten file")
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(modified)
                    .set_accessed(accessed),
            )
            .expect("restore timestamps");
        assert!(
            !binding
                .workspace_matches(&canonical)
                .await
                .expect("stale content comparison"),
            "same-length content drift with restored mtime must block Execute"
        );

        let current_change_token =
            crate::internal::ai::workspace_snapshot::workspace_snapshot_metadata_fingerprint(
                &canonical,
            )
            .expect("current metadata change token");
        let mut matching_hint = binding.clone();
        matching_hint.workspace_change_token =
            format!("{PHASE1_WORKSPACE_METADATA_FINGERPRINT_PREFIX}{current_change_token}");
        assert!(
            matching_hint
                .workspace_change_matches(&canonical)
                .await
                .expect("matching metadata hint"),
            "the additive metadata token should match the current workspace hint"
        );
        assert!(
            !matching_hint
                .workspace_matches(&canonical)
                .await
                .expect("content authority after matching hint"),
            "a matching metadata hint must never authorize stale content"
        );

        assert!(
            !legacy
                .workspace_change_matches(&canonical)
                .await
                .expect("legacy drift comparison"),
            "legacy contexts without a change token must fall back to content hashing"
        );
    }

    #[tokio::test]
    async fn phase1_capture_rejects_change_between_content_and_metadata_scans() {
        let temp = tempfile::tempdir().expect("temp dir");
        test::setup_with_new_libra_in(temp.path()).await;
        let canonical = std::fs::canonicalize(temp.path()).expect("canonical workspace");
        let path = canonical.join("README.md");
        std::fs::write(&path, "before\n").expect("write baseline");
        let intent_spec = resolve_intentspec(
            IntentDraft {
                intent: DraftIntent {
                    summary: "Capture a stable Phase 1 workspace".to_string(),
                    problem_statement: "Mixed fingerprint baselines must be rejected".to_string(),
                    change_type: ChangeType::Test,
                    objectives: vec![Objective {
                        title: "Reject a mixed workspace baseline".to_string(),
                        kind: ObjectiveKind::Analysis,
                    }],
                    in_scope: vec!["README.md".to_string()],
                    out_of_scope: Vec::new(),
                    touch_hints: None,
                },
                acceptance: DraftAcceptance {
                    success_criteria: vec!["No inconsistent binding is returned".to_string()],
                    fast_checks: Vec::new(),
                    integration_checks: Vec::new(),
                    security_checks: Vec::new(),
                    release_checks: Vec::new(),
                },
                risk: DraftRisk {
                    rationale: "test fixture".to_string(),
                    factors: Vec::new(),
                    level: Some(RiskLevel::Low),
                },
            },
            RiskLevel::Low,
            ResolveContext {
                working_dir: canonical.to_string_lossy().into_owned(),
                base_ref: "HEAD".to_string(),
                created_by_id: "phase1-stable-capture-test".to_string(),
            },
        );
        let changed_path = path.clone();
        let metadata = std::fs::metadata(&path).expect("baseline metadata");
        let modified = metadata.modified().expect("baseline modified time");
        let accessed = metadata.accessed().expect("baseline accessed time");

        let error = Phase1CheckoutBinding::capture_with_post_content_hook(
            &canonical,
            &intent_spec,
            move || {
                std::fs::write(&changed_path, "after!\n")?;
                std::fs::File::options()
                    .write(true)
                    .open(&changed_path)?
                    .set_times(
                        std::fs::FileTimes::new()
                            .set_modified(modified)
                            .set_accessed(accessed),
                    )
            },
        )
        .await
        .expect_err("capture must reject a mixed content/metadata baseline");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("content and metadata fingerprints were captured"),
            "unexpected stable-capture error: {error}"
        );
    }

    #[tokio::test]
    async fn phase1_exact_validation_rejects_identity_change_after_content_scan() {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let temp = tempfile::tempdir().expect("temp dir");
        test::setup_with_new_libra_in(temp.path()).await;
        let canonical = std::fs::canonicalize(temp.path()).expect("canonical workspace");
        std::fs::write(canonical.join("README.md"), "stable content\n")
            .expect("write stable workspace content");
        let intent_spec = resolve_intentspec(
            IntentDraft {
                intent: DraftIntent {
                    summary: "Guard exact Phase 1 identity".to_string(),
                    problem_statement: "HEAD must not change around exact workspace authorization"
                        .to_string(),
                    change_type: ChangeType::Test,
                    objectives: vec![Objective {
                        title: "Reject a post-scan HEAD move".to_string(),
                        kind: ObjectiveKind::Analysis,
                    }],
                    in_scope: vec!["README.md".to_string()],
                    out_of_scope: Vec::new(),
                    touch_hints: None,
                },
                acceptance: DraftAcceptance {
                    success_criteria: vec!["The Execute boundary fails closed".to_string()],
                    fast_checks: Vec::new(),
                    integration_checks: Vec::new(),
                    security_checks: Vec::new(),
                    release_checks: Vec::new(),
                },
                risk: DraftRisk {
                    rationale: "test fixture".to_string(),
                    factors: Vec::new(),
                    level: Some(RiskLevel::Low),
                },
            },
            RiskLevel::Low,
            ResolveContext {
                working_dir: canonical.to_string_lossy().into_owned(),
                base_ref: "HEAD".to_string(),
                created_by_id: "phase1-exact-identity-test".to_string(),
            },
        );
        let binding = Phase1CheckoutBinding::capture(&canonical, &intent_spec)
            .await
            .expect("capture baseline Phase 1 binding");
        let db_path = canonical
            .join(crate::utils::util::ROOT_DIR)
            .join(crate::utils::util::DATABASE);

        let error = binding
            .validate_exact_with_post_scan_hook(&canonical, &intent_spec, move || async move {
                let db = crate::internal::db::get_db_conn_instance_for_path(&db_path).await?;
                let head = crate::internal::model::reference::Entity::find()
                    .filter(
                        crate::internal::model::reference::Column::Kind
                            .eq(crate::internal::model::reference::ConfigKind::Head),
                    )
                    .filter(crate::internal::model::reference::Column::Remote.is_null())
                    .filter(crate::internal::model::reference::Column::WorktreeId.is_null())
                    .one(&db)
                    .await
                    .map_err(|error| {
                        std::io::Error::other(format!("failed to load HEAD test row: {error}"))
                    })?
                    .ok_or_else(|| std::io::Error::other("HEAD test row is missing"))?;
                let mut head: crate::internal::model::reference::ActiveModel = head.into();
                head.name = Set(Some("moved-after-scan".to_string()));
                head.update(&db).await.map_err(|error| {
                    std::io::Error::other(format!("failed to move HEAD in test hook: {error}"))
                })?;
                Ok(())
            })
            .await
            .expect_err("a HEAD move after the content scan must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("checkout identity changed"),
            "unexpected exact-validation error: {error}"
        );
    }

    fn phase1_durability_test_context(interaction_id: &str) -> Phase1ReviewContext {
        let intent_spec = resolve_intentspec(
            IntentDraft {
                intent: DraftIntent {
                    summary: "Exercise Phase 1 durability".to_string(),
                    problem_statement: "Phase 1 sidecars must survive ambiguous syncs".to_string(),
                    change_type: ChangeType::Test,
                    objectives: vec![Objective {
                        title: "Keep recovery authority durable".to_string(),
                        kind: ObjectiveKind::Analysis,
                    }],
                    in_scope: vec!["src/internal/ai/runtime/phase1.rs".to_string()],
                    out_of_scope: Vec::new(),
                    touch_hints: None,
                },
                acceptance: DraftAcceptance {
                    success_criteria: vec!["Retries re-sync visible state".to_string()],
                    fast_checks: Vec::new(),
                    integration_checks: Vec::new(),
                    security_checks: Vec::new(),
                    release_checks: Vec::new(),
                },
                risk: DraftRisk {
                    rationale: "test fixture".to_string(),
                    factors: Vec::new(),
                    level: Some(RiskLevel::Low),
                },
            },
            RiskLevel::Low,
            ResolveContext {
                working_dir: "/repo/main".to_string(),
                base_ref: "HEAD".to_string(),
                created_by_id: "phase1-durability-test".to_string(),
            },
        );
        let intent_spec_id = intent_spec.metadata.id.clone();
        Phase1ReviewContext {
            schema_version: Phase1ReviewContext::SCHEMA_VERSION,
            interaction_id: interaction_id.to_string(),
            intent_id: "phase1-durability-intent".to_string(),
            intent_spec_id: intent_spec_id.clone(),
            persisted_plan: Phase1PersistedPlan::Unavailable,
            intent_spec,
            plan_draft: crate::internal::ai::tools::context::SubmitPlanDraftArgs {
                explanation: None,
                steps: vec![crate::internal::ai::tools::context::PlanDraftStep {
                    title: "Keep recovery authority durable".to_string(),
                }],
            },
            execution_plan: ExecutionPlanSpec {
                intent_spec_id,
                revision: 1,
                parent_revision: None,
                replan_reason: None,
                tasks: Vec::new(),
                max_parallel: 1,
                checkpoints: Vec::new(),
            },
            default_allow_network: false,
            checkout: Phase1CheckoutBinding {
                canonical_working_dir: "/repo/main".to_string(),
                repo_id: "repo-id".to_string(),
                repo_locator: "/repo/main".to_string(),
                base_ref: "HEAD".to_string(),
                workspace_fingerprint: "0".repeat(64),
                workspace_change_token: String::new(),
                head_oid: Some("0".repeat(40)),
                branch_label: "main".to_string(),
                worktree_id: None,
            },
        }
    }

    fn phase1_durability_test_seed() -> Phase1StartSeed {
        let context = phase1_durability_test_context("seed-context-template");
        Phase1StartSeed {
            schema_version: Phase1StartSeed::SCHEMA_VERSION,
            attempt_id: "phase1-durability-attempt".to_string(),
            source_interaction_id: "intent-review".to_string(),
            intent_id: context.intent_id,
            intent_spec_id: context.intent_spec_id,
            intent_spec_json: serde_json::to_string(&context.intent_spec)
                .expect("serialize durability test IntentSpec"),
            source_resolution: "confirm".to_string(),
            revision_note: None,
            checkout: context.checkout,
            prior_plan: None,
            prior_plan_id: None,
            prior_persisted_plan: Phase1PersistedPlan::Unavailable,
            browser_command_id: None,
        }
    }

    #[test]
    fn embedded_phase1_retry_gate_tracks_exact_seed_lineage_and_resolution() {
        use crate::internal::ai::session::{
            CodeCommandIdentity, CodeCommandIntent, CodeWorkflowEventKind, Phase1RetryIntentReview,
        };

        let seed = phase1_durability_test_seed();
        let phase1_turn_id = phase1_turn_id_from_seed(&seed).expect("derive Phase 1 turn id");
        let command = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", &phase1_turn_id),
            "headless_direct_turn",
            "sha256:input",
            true,
        );
        let retry = Phase1RetryIntentReview {
            interaction_id: "intent-review-retry".to_string(),
            intent_id: seed.intent_id.clone(),
            intent_spec_id: seed.intent_spec_id.clone(),
            source_interaction_id: seed.source_interaction_id.clone(),
            source_resolution: seed.source_resolution.clone(),
            source_phase1_turn_id: phase1_turn_id.clone(),
            start_seed_digest: seed.durable_digest().expect("seed digest"),
        };
        let intent = CodeWorkflowEventKind::CommandIntentPersisted {
            command: command.clone(),
        };
        let failed = CodeWorkflowEventKind::CommandTerminalFailure {
            command: command.identity,
            reason: "planner failed before formal write".to_string(),
            interaction_resolutions: Vec::new(),
            retry_intent_review: Some(retry.clone()),
        };

        assert_eq!(
            phase1_retry_intent_review_state([&intent, &failed], &phase1_turn_id)
                .expect("scan embedded retry"),
            Phase1RetryIntentReviewState::Open(retry.clone())
        );
        validate_phase1_retry_intent_review_for_seed(&retry, &phase1_turn_id, &seed)
            .expect("retry lineage matches seed");
        assert_eq!(
            super::super::phase0::open_intent_review_from_workflow([&intent, &failed]),
            Some((
                retry.interaction_id.clone(),
                retry.intent_id.clone(),
                String::new(),
                String::new(),
            ))
        );
        let binding = CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: retry.interaction_id.clone(),
            intent_id: retry.intent_id.clone(),
            turn_id: "intent-review-restore-stable".to_string(),
            phase0_turn_id: String::new(),
        };
        assert_eq!(
            super::super::phase0::open_intent_review_from_workflow([&intent, &failed, &binding]),
            Some((
                retry.interaction_id.clone(),
                retry.intent_id.clone(),
                "intent-review-restore-stable".to_string(),
                String::new(),
            ))
        );

        let resolved = CodeWorkflowEventKind::InteractionResolved {
            interaction_id: retry.interaction_id.clone(),
            resolution: "cancel".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        };
        assert_eq!(
            phase1_retry_intent_review_state(
                [&intent, &failed, &binding, &resolved],
                &phase1_turn_id,
            )
            .expect("scan resolved retry"),
            Phase1RetryIntentReviewState::Resolved {
                review: retry,
                resolution: "cancel".to_string(),
            }
        );
        assert_eq!(
            super::super::phase0::open_intent_review_from_workflow([
                &intent, &failed, &binding, &resolved
            ]),
            None
        );
    }

    #[test]
    fn exact_phase1_seed_retry_resyncs_file_and_parent_after_visible_replace() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp_dir.path().to_path_buf(),
        );
        let seed = phase1_durability_test_seed();

        store.fail_next_phase1_seed_parent_sync_for_test();
        persist_phase1_start_seed_idempotent(&store, &seed)
            .expect_err("first write stops after replacement and before parent sync");
        let visible_seed = load_phase1_start_seed(&store)
            .expect("visible seed remains readable")
            .expect("visible seed");
        assert_eq!(
            visible_seed.durable_digest().expect("visible seed digest"),
            seed.durable_digest().expect("expected seed digest")
        );

        store.fail_next_phase1_seed_parent_sync_for_test();
        persist_phase1_start_seed_idempotent(&store, &seed)
            .expect_err("exact retry must re-sync the visible seed parent");
        persist_phase1_start_seed_idempotent(&store, &seed)
            .expect("third attempt re-syncs the exact seed before ACK");
    }

    #[test]
    fn missing_phase1_seed_remove_retry_resyncs_existing_parent() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp_dir.path().to_path_buf(),
        );
        let seed = phase1_durability_test_seed();
        persist_phase1_start_seed(&store, &seed).expect("persist test seed");

        store.fail_next_phase1_seed_sync_after_remove_for_test();
        clear_phase1_start_seed(&store)
            .expect_err("first removal stops after unlink and before parent sync");
        assert!(!phase1_start_seed_path(&store).exists());

        store.fail_next_phase1_seed_sync_after_remove_for_test();
        clear_phase1_start_seed(&store)
            .expect_err("NotFound retry must still attempt to sync the existing parent");
        clear_phase1_start_seed(&store)
            .expect("third removal durably acknowledges the already-absent seed");
    }

    #[test]
    fn startup_syncs_visible_terminal_row_before_context_gc() {
        use crate::internal::ai::session::jsonl::{
            CodeCommandIdentity, CodeCommandIntent, CodeWorkflowEventKind,
        };

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp_dir.path().to_path_buf(),
        );
        let context = phase1_durability_test_context("startup-review");
        persist_phase1_review_context(&store, &context).expect("persist review context");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id: context.interaction_id.clone(),
                plan_id: "plan-1".to_string(),
                turn_id: "plan-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: context.interaction_id.clone(),
                revision_of: None,
                prepared_from_network: None,
            })
            .expect("open review gate");
        let command = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", "cancel-review"),
            "review-response",
            "cancel-review-hash",
            false,
        );
        store
            .admit_code_command(command.clone())
            .expect("admit review response");

        store.fail_next_durable_sync_after_write_for_test();
        store
            .complete_code_command_success_with_interaction_resolved(
                &command.identity,
                "review cancelled",
                &context.interaction_id,
                "cancel",
            )
            .expect_err("terminal row is visible but its sync fails");
        let context_path = phase1_review_context_path(&store, &context.interaction_id);
        assert!(context_path.is_file());

        store.fail_next_events_log_resync_for_test();
        prepare_phase1_recovery_authority(&store)
            .expect_err("startup must fail before GC when the event log cannot be re-synced");
        assert!(
            context_path.is_file(),
            "an uncommitted terminal row must not authorize context deletion"
        );

        assert_eq!(
            prepare_phase1_recovery_authority(&store)
                .expect("successful startup re-sync permits authority validation and GC"),
            1
        );
        assert!(!context_path.exists());
    }

    #[test]
    fn phase1_recovery_rejects_sequence_gap_without_revision_sidecar_before_gc() {
        use crate::internal::ai::session::CodeWorkflowEventKind;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp_dir.path().to_path_buf(),
        );
        let open_context = phase1_durability_test_context("gap-open-review");
        let orphan_context = phase1_durability_test_context("gap-orphan-review");
        persist_phase1_review_context(&store, &open_context).expect("persist open context");
        persist_phase1_review_context(&store, &orphan_context).expect("persist orphan context");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id: open_context.interaction_id.clone(),
                plan_id: "gap-plan".to_string(),
                turn_id: "gap-plan-gate".to_string(),
                phase1_turn_id: "gap-phase1-turn".to_string(),
                context_id: open_context.interaction_id.clone(),
                revision_of: None,
                prepared_from_network: None,
            })
            .expect("append open plan gate");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "gap-middle".to_string(),
                summary: "row removed by corruption fixture".to_string(),
                payload: serde_json::Value::Null,
            })
            .expect("append removable middle row");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::TerminalSuccess {
                command_id: "gap-tail".to_string(),
                summary: "retain a later sequence".to_string(),
            })
            .expect("append retained tail row");

        let events_path = store.events_path();
        let intact = std::fs::read_to_string(&events_path).expect("read intact workflow log");
        let lines = intact.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        let corrupt = format!("{}\n{}\n", lines[0], lines[2]);
        std::fs::write(&events_path, &corrupt).expect("install sequence gap");
        let open_path = phase1_review_context_path(&store, &open_context.interaction_id);
        let orphan_path = phase1_review_context_path(&store, &orphan_context.interaction_id);

        let error = prepare_phase1_recovery_authority(&store)
            .expect_err("ordinary Plan recovery must reject an incomplete replay");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("complete Code workflow replay"));
        assert_eq!(
            std::fs::read_to_string(&events_path).expect("re-read workflow log"),
            corrupt,
            "strict replay preflight must not rewrite the workflow log"
        );
        assert!(open_path.is_file());
        assert!(
            orphan_path.is_file(),
            "strict replay preflight must run before irreversible context GC"
        );
    }

    #[test]
    fn phase1_context_budget_counts_legacy_root_sidecars_and_recovers_after_gc() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp_dir.path().to_path_buf(),
        );
        for index in 0..MAX_PHASE1_CONTEXT_FILES {
            let path = phase1_review_context_path(&store, &format!("review-{index}"));
            std::fs::create_dir_all(path.parent().expect("phase1 parent"))
                .expect("create phase1 root");
            std::fs::write(path, []).expect("write context-shaped sidecar");
        }
        let error = validate_phase1_context_budget_for_bytes(&store, 1)
            .expect_err("the 65th context must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);

        clear_phase1_review_context(&store, "review-0").expect("durably remove old context");
        validate_phase1_context_budget_for_bytes(&store, 1)
            .expect("one GC slot permits the replacement context");
    }

    #[test]
    fn phase1_context_budget_rejects_total_bytes_at_preflight() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp_dir.path().to_path_buf(),
        );
        let path = phase1_review_context_path(&store, "large-review");
        std::fs::create_dir_all(path.parent().expect("phase1 parent")).expect("create phase1 root");
        let file = std::fs::File::create(path).expect("create sparse context sidecar");
        file.set_len(MAX_PHASE1_CONTEXT_TOTAL_BYTES)
            .expect("size sparse context sidecar");

        let error = validate_phase1_context_budget_for_bytes(&store, 1)
            .expect_err("aggregate context bytes must be bounded");
        assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    }

    #[test]
    fn back_prepare_is_provisional_until_network_back_is_durable() {
        use crate::internal::ai::session::CodeWorkflowEventKind as Kind;

        let events = vec![
            Kind::PlanReviewRequested {
                interaction_id: "plan-review".to_string(),
                plan_id: "plan-1".to_string(),
                turn_id: "plan-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: "plan-review".to_string(),
                revision_of: None,
                prepared_from_network: None,
            },
            Kind::InteractionResolved {
                interaction_id: "plan-review".to_string(),
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
            Kind::NetworkPolicyRequested {
                interaction_id: "network-review".to_string(),
                plan_id: "plan-1".to_string(),
                turn_id: "network-turn".to_string(),
                default_allow: false,
            },
            Kind::PlanReviewRequested {
                interaction_id: "plan-review-back".to_string(),
                plan_id: "plan-1".to_string(),
                turn_id: "back-plan-turn".to_string(),
                phase1_turn_id: String::new(),
                context_id: "plan-review".to_string(),
                revision_of: None,
                prepared_from_network: Some("network-review".to_string()),
            },
        ];
        assert!(open_plan_review_from_workflow(events.iter()).is_none());
        assert_eq!(
            open_network_policy_from_workflow(events.iter()).map(|(id, ..)| id),
            Some("network-review".to_string())
        );
        let mut activated = events;
        activated.push(Kind::InteractionResolved {
            interaction_id: "network-review".to_string(),
            resolution: "back".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        });
        assert_eq!(
            open_plan_review_from_workflow(activated.iter()).map(|(id, ..)| id),
            Some("plan-review-back".to_string())
        );
        assert!(open_network_policy_from_workflow(activated.iter()).is_none());
    }

    #[test]
    fn legacy_empty_gate_turn_bindings_replace_in_place_for_stable_resume() {
        use crate::internal::ai::session::CodeWorkflowEventKind as Kind;

        let legacy_plan = Kind::PlanReviewRequested {
            interaction_id: "plan-review".to_string(),
            plan_id: "plan-1".to_string(),
            turn_id: String::new(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "context-1".to_string(),
            revision_of: None,
            prepared_from_network: None,
        };
        let bound_plan = Kind::PlanReviewRequested {
            interaction_id: "plan-review".to_string(),
            plan_id: "plan-1".to_string(),
            turn_id: "plan-review-restore-stable".to_string(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "context-1".to_string(),
            revision_of: None,
            prepared_from_network: None,
        };
        assert_eq!(
            open_plan_review_from_workflow([&legacy_plan, &bound_plan]),
            Some((
                "plan-review".to_string(),
                "plan-1".to_string(),
                "plan-review-restore-stable".to_string(),
                "phase1-turn".to_string(),
            ))
        );

        let plan_resolution = Kind::InteractionResolved {
            interaction_id: "plan-review".to_string(),
            resolution: "execute".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        };
        let legacy_network = Kind::NetworkPolicyRequested {
            interaction_id: "network-review".to_string(),
            plan_id: "plan-1".to_string(),
            turn_id: String::new(),
            default_allow: false,
        };
        let bound_network = Kind::NetworkPolicyRequested {
            interaction_id: "network-review".to_string(),
            plan_id: "plan-1".to_string(),
            turn_id: "network-review-restore-stable".to_string(),
            default_allow: false,
        };
        assert_eq!(
            open_network_policy_from_workflow([
                &legacy_plan,
                &legacy_network,
                &plan_resolution,
                &bound_network,
            ]),
            Some((
                "network-review".to_string(),
                "plan-1".to_string(),
                "network-review-restore-stable".to_string(),
                false,
            ))
        );
    }

    #[test]
    fn back_retry_rollback_cannot_close_activated_replacement_plan() {
        use crate::internal::ai::session::CodeWorkflowEventKind as Kind;

        let plan = Kind::PlanReviewRequested {
            interaction_id: "plan-1".to_string(),
            plan_id: "persisted-plan".to_string(),
            turn_id: "plan-turn".to_string(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "context-1".to_string(),
            revision_of: None,
            prepared_from_network: None,
        };
        let network = Kind::NetworkPolicyRequested {
            interaction_id: "network-1".to_string(),
            plan_id: "persisted-plan".to_string(),
            turn_id: "network-turn".to_string(),
            default_allow: false,
        };
        let back = Kind::PlanReviewRequested {
            interaction_id: "plan-back-1".to_string(),
            plan_id: "persisted-plan".to_string(),
            turn_id: "plan-back-turn".to_string(),
            phase1_turn_id: String::new(),
            context_id: "context-1".to_string(),
            revision_of: None,
            prepared_from_network: Some("network-1".to_string()),
        };
        let events = [
            plan,
            network,
            Kind::InteractionResolved {
                interaction_id: "plan-1".to_string(),
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
            back.clone(),
            Kind::InteractionResolved {
                interaction_id: "network-1".to_string(),
                resolution: "back".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
            back,
            Kind::InteractionResolved {
                interaction_id: "plan-back-1".to_string(),
                resolution: "back-prepare-rollback".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
        ];
        assert_eq!(
            open_plan_review_from_workflow(events.iter()).map(|(id, ..)| id),
            Some("plan-back-1".to_string())
        );
        assert!(open_network_policy_from_workflow(events.iter()).is_none());
    }

    #[test]
    fn execute_retry_rollback_cannot_close_activated_network_gate() {
        use crate::internal::ai::session::CodeWorkflowEventKind as Kind;

        let network = Kind::NetworkPolicyRequested {
            interaction_id: "network-1".to_string(),
            plan_id: "persisted-plan".to_string(),
            turn_id: "network-turn".to_string(),
            default_allow: false,
        };
        let events = [
            Kind::PlanReviewRequested {
                interaction_id: "plan-1".to_string(),
                plan_id: "persisted-plan".to_string(),
                turn_id: "plan-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: "context-1".to_string(),
                revision_of: None,
                prepared_from_network: None,
            },
            network.clone(),
            Kind::InteractionResolved {
                interaction_id: "plan-1".to_string(),
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
            network,
            Kind::InteractionResolved {
                interaction_id: "network-1".to_string(),
                resolution: "network-prepare-rollback".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
        ];
        assert_eq!(
            open_network_policy_from_workflow(events.iter()).map(|(id, ..)| id),
            Some("network-1".to_string())
        );
    }

    #[test]
    fn late_modify_resolution_cannot_rearm_consumed_revision_source() {
        use crate::internal::ai::session::CodeWorkflowEventKind as Kind;

        let events = [
            Kind::PlanReviewRequested {
                interaction_id: "source-plan".to_string(),
                plan_id: "plan-1".to_string(),
                turn_id: "source-turn".to_string(),
                phase1_turn_id: "phase1-source".to_string(),
                context_id: "source-plan".to_string(),
                revision_of: None,
                prepared_from_network: None,
            },
            Kind::InteractionResolved {
                interaction_id: "source-plan".to_string(),
                resolution: "modify".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
            Kind::PlanReviewRequested {
                interaction_id: "replacement-plan".to_string(),
                plan_id: "plan-2".to_string(),
                turn_id: "replacement-turn".to_string(),
                phase1_turn_id: "phase1-replacement".to_string(),
                context_id: "replacement-plan".to_string(),
                revision_of: Some("source-plan".to_string()),
                prepared_from_network: None,
            },
            Kind::InteractionResolved {
                interaction_id: "source-plan".to_string(),
                resolution: "modify".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
        ];

        assert!(pending_plan_revision_from_workflow(events.iter()).is_none());
    }

    #[test]
    fn revision_step_provenance_requires_complete_semantic_equality() {
        let actor = ActorRef::agent("revision-provenance-test").expect("actor");
        let mut task = GitTask::new(actor, "Edit source", None).expect("task");
        let step = PlanStep::new("Edit source");
        task.set_origin_step_id(Some(step.step_id()));
        let prior = ExecutionPlanSpec {
            intent_spec_id: "intent-1".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![TaskSpec {
                step,
                task,
                objective: "Update source".to_string(),
                kind: TaskKind::Implementation,
                gate_stage: None,
                owner_role: Some("coder".to_string()),
                scope_in: vec!["src/".to_string()],
                scope_out: vec![],
                checks: vec![],
                contract: TaskContract::default(),
            }],
            max_parallel: 1,
            checkpoints: vec![],
        };
        let prior_step_id = prior.tasks[0].step_id();

        let mut unchanged = prior.clone();
        unchanged.tasks[0].step = PlanStep::new("Edit source");
        let unchanged_step_id = unchanged.tasks[0].step_id();
        unchanged.tasks[0]
            .task
            .set_origin_step_id(Some(unchanged_step_id));
        assert_ne!(unchanged.tasks[0].step_id(), prior_step_id);
        preserve_unchanged_revision_steps(&mut unchanged, &prior);
        assert_eq!(unchanged.tasks[0].step_id(), prior_step_id);

        let mut changed = prior.clone();
        changed.tasks[0].step = PlanStep::new("Edit source");
        let changed_step_id = changed.tasks[0].step_id();
        changed.tasks[0]
            .task
            .set_origin_step_id(Some(changed_step_id));
        changed.tasks[0].scope_out.push("docs/".to_string());
        preserve_unchanged_revision_steps(&mut changed, &prior);
        assert_eq!(changed.tasks[0].step_id(), changed_step_id);
        assert_ne!(changed.tasks[0].step_id(), prior_step_id);
    }

    /// Startup recovery may complete exactly one draft-writing mutation. With
    /// only a Plan review open, that is the Phase 1 turn; once an IntentSpec
    /// review is also open the Phase 0 turn wins, because no plan can have been
    /// approved while the intent itself is still unconfirmed.
    #[test]
    fn open_review_gate_phase_turn_id_prefers_phase0_then_phase1() {
        use crate::internal::ai::session::{
            CodeWorkflowEventKind as Kind, jsonl::SessionJsonlStore,
        };

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SessionJsonlStore::new(temp_dir.path().to_path_buf());
        assert_eq!(
            open_review_gate_phase_turn_id(&store),
            None,
            "an empty workflow log must keep every pending mutation fenced"
        );

        store
            .append_code_workflow_durable(Kind::PlanReviewRequested {
                interaction_id: "plan-1".to_string(),
                plan_id: "plan-1".to_string(),
                turn_id: "plan-review-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: "plan-1".to_string(),
                revision_of: None,
                prepared_from_network: None,
            })
            .expect("append plan review marker");
        assert_eq!(
            open_review_gate_phase_turn_id(&store).as_deref(),
            Some("phase1-turn")
        );

        store
            .append_code_workflow_durable(Kind::IntentReviewRequested {
                interaction_id: "intent-1".to_string(),
                intent_id: "intent-1".to_string(),
                turn_id: "intent-review-turn".to_string(),
                phase0_turn_id: "phase0-turn".to_string(),
            })
            .expect("append intent review marker");
        assert_eq!(
            open_review_gate_phase_turn_id(&store).as_deref(),
            Some("phase0-turn")
        );

        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "intent-1".to_string(),
                resolution: "confirm".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("append intent resolution");
        assert_eq!(
            open_review_gate_phase_turn_id(&store).as_deref(),
            Some("phase1-turn"),
            "resolving the IntentSpec gate falls back to the still-open Plan gate"
        );

        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "plan-1".to_string(),
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("append plan resolution");
        assert_eq!(
            open_review_gate_phase_turn_id(&store),
            None,
            "with no gate open, recovery must fence every pending mutation"
        );
    }

    /// The network-policy marker must outlive the Plan review it follows: the
    /// crash window between "plan approved" and "network policy answered" is
    /// exactly the case where the plan marker is already resolved, so only the
    /// network marker can prove a human gate is still owed.
    #[test]
    fn open_network_policy_survives_resolved_plan_review() {
        use crate::internal::ai::session::{
            CodeWorkflowEventKind as Kind, jsonl::SessionJsonlStore,
        };

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SessionJsonlStore::new(temp_dir.path().to_path_buf());
        let replay_events = |store: &SessionJsonlStore| {
            store
                .load_code_workflow_replay()
                .expect("replay workflow log")
                .events
                .into_iter()
                .map(|event| event.event)
                .collect::<Vec<_>>()
        };

        store
            .append_code_workflow_durable(Kind::PlanReviewRequested {
                interaction_id: "plan-9".to_string(),
                plan_id: "plan-9".to_string(),
                turn_id: "plan-review-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: "plan-9".to_string(),
                revision_of: None,
                prepared_from_network: None,
            })
            .expect("append plan review marker");
        let events = replay_events(&store);
        assert!(
            open_network_policy_from_workflow(events.iter()).is_none(),
            "a plan review alone must not look like an open network gate"
        );

        // Execute writes the network marker *before* resolving the plan gate.
        store
            .append_code_workflow_durable(Kind::NetworkPolicyRequested {
                interaction_id: "plan-9:network-policy".to_string(),
                plan_id: "plan-9".to_string(),
                turn_id: "network-policy-turn".to_string(),
                default_allow: true,
            })
            .expect("append network policy marker");
        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "plan-9".to_string(),
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("append plan resolution");

        let events = replay_events(&store);
        assert_eq!(
            open_plan_review_from_workflow(events.iter()),
            None,
            "the plan review is resolved once Execute is delivered"
        );
        assert_eq!(
            open_network_policy_from_workflow(events.iter()),
            Some((
                "plan-9:network-policy".to_string(),
                "plan-9".to_string(),
                "network-policy-turn".to_string(),
                true,
            )),
            "the unanswered network gate must survive the resolved plan review"
        );

        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "plan-9:network-policy".to_string(),
                resolution: "network-allow".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("append network policy resolution");
        let events = replay_events(&store);
        assert!(
            open_network_policy_from_workflow(events.iter()).is_none(),
            "answering the gate must clear the durable marker"
        );
    }

    #[test]
    fn open_network_policy_survives_execute_with_empty_plan_id() {
        use crate::internal::ai::session::{
            CodeWorkflowEventKind as Kind, jsonl::SessionJsonlStore,
        };

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SessionJsonlStore::new(temp_dir.path().to_path_buf());
        let replay_events = |store: &SessionJsonlStore| {
            store
                .load_code_workflow_replay()
                .expect("replay workflow log")
                .events
                .into_iter()
                .map(|event| event.event)
                .collect::<Vec<_>>()
        };

        // Persist/MCP failure path records an empty plan_id on both markers.
        store
            .append_code_workflow_durable(Kind::PlanReviewRequested {
                interaction_id: "review-empty".to_string(),
                plan_id: String::new(),
                turn_id: "plan-review-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: "review-empty".to_string(),
                revision_of: None,
                prepared_from_network: None,
            })
            .expect("append plan review marker");
        store
            .append_code_workflow_durable(Kind::NetworkPolicyRequested {
                interaction_id: network_policy_interaction_id(None),
                plan_id: String::new(),
                turn_id: "network-policy-turn".to_string(),
                default_allow: false,
            })
            .expect("append network policy marker");
        let events = replay_events(&store);
        assert!(
            open_network_policy_from_workflow(events.iter()).is_none(),
            "empty-plan network marker must not restore while plan review is open"
        );

        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "review-empty".to_string(),
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("append plan resolution");
        let events = replay_events(&store);
        assert_eq!(
            open_network_policy_from_workflow(events.iter()),
            Some((
                network_policy_interaction_id(None),
                String::new(),
                "network-policy-turn".to_string(),
                false,
            )),
            "Execute with empty plan_id must still restore the network gate"
        );
    }

    #[test]
    fn open_network_policy_demotes_when_plan_review_reopens_after_back() {
        use crate::internal::ai::session::{
            CodeWorkflowEventKind as Kind, jsonl::SessionJsonlStore,
        };

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SessionJsonlStore::new(temp_dir.path().to_path_buf());
        let replay_events = |store: &SessionJsonlStore| {
            store
                .load_code_workflow_replay()
                .expect("replay workflow log")
                .events
                .into_iter()
                .map(|event| event.event)
                .collect::<Vec<_>>()
        };

        store
            .append_code_workflow_durable(Kind::PlanReviewRequested {
                interaction_id: "review-back".to_string(),
                plan_id: "plan-back".to_string(),
                turn_id: "plan-review-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: "review-back".to_string(),
                revision_of: None,
                prepared_from_network: None,
            })
            .expect("plan review");
        store
            .append_code_workflow_durable(Kind::NetworkPolicyRequested {
                interaction_id: "plan-back:network-policy".to_string(),
                plan_id: "plan-back".to_string(),
                turn_id: "network-policy-turn".to_string(),
                default_allow: true,
            })
            .expect("network marker");
        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "review-back".to_string(),
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("execute");
        assert_eq!(
            open_network_policy_from_workflow(replay_events(&store).iter()).map(|(id, ..)| id),
            Some("plan-back:network-policy".to_string()),
            "network gate is restorable after Execute"
        );

        // Crash after Back appends a replacement PlanReviewRequested but before
        // the network interaction is resolved.
        store
            .append_code_workflow_durable(Kind::PlanReviewRequested {
                interaction_id: "review-back-generation-2".to_string(),
                plan_id: "plan-back".to_string(),
                turn_id: "plan-review-turn-2".to_string(),
                phase1_turn_id: String::new(),
                context_id: "review-back".to_string(),
                revision_of: None,
                prepared_from_network: Some("plan-back:network-policy".to_string()),
            })
            .expect("back re-opens plan review");
        assert_eq!(
            open_network_policy_from_workflow(replay_events(&store).iter()).map(|(id, ..)| id),
            Some("plan-back:network-policy".to_string()),
            "provisional Back must preserve the current network gate"
        );
        assert!(open_plan_review_from_workflow(replay_events(&store).iter()).is_none());
        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "plan-back:network-policy".to_string(),
                resolution: "back".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("back");
        assert_eq!(
            open_plan_review_from_workflow(replay_events(&store).iter()).map(|(id, ..)| id),
            Some("review-back-generation-2".to_string()),
            "durable Back activates the replacement Plan gate"
        );
        assert!(open_network_policy_from_workflow(replay_events(&store).iter()).is_none());
    }

    /// The marker is written before Plan `Execute` is delivered, so a crash in
    /// that window leaves it next to a still-open plan review. The plan was
    /// never approved then, so the gate must stay unrestorable until a durable
    /// Execute resolution exists — otherwise the network dialog would stand in
    /// for an approval that never happened.
    #[test]
    fn open_network_policy_waits_for_a_durable_plan_execute() {
        use crate::internal::ai::session::{
            CodeWorkflowEventKind as Kind, jsonl::SessionJsonlStore,
        };

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SessionJsonlStore::new(temp_dir.path().to_path_buf());
        let replay_events = |store: &SessionJsonlStore| {
            store
                .load_code_workflow_replay()
                .expect("replay workflow log")
                .events
                .into_iter()
                .map(|event| event.event)
                .collect::<Vec<_>>()
        };

        store
            .append_code_workflow_durable(Kind::PlanReviewRequested {
                interaction_id: "review-11".to_string(),
                plan_id: "plan-11".to_string(),
                turn_id: "plan-review-turn".to_string(),
                phase1_turn_id: "phase1-turn".to_string(),
                context_id: "review-11".to_string(),
                revision_of: None,
                prepared_from_network: None,
            })
            .expect("append plan review marker");
        store
            .append_code_workflow_durable(Kind::NetworkPolicyRequested {
                interaction_id: "plan-11:network-policy".to_string(),
                plan_id: "plan-11".to_string(),
                turn_id: "network-policy-turn".to_string(),
                default_allow: false,
            })
            .expect("append network policy marker");

        let events = replay_events(&store);
        assert!(
            open_network_policy_from_workflow(events.iter()).is_none(),
            "a network marker must not open a gate while its plan review is unresolved"
        );
        assert!(
            open_plan_review_from_workflow(events.iter()).is_some(),
            "the plan review still owns the session in this crash window"
        );

        // Cancelling the plan must not promote the marker either.
        store
            .append_code_workflow_durable(Kind::InteractionResolved {
                interaction_id: "review-11".to_string(),
                resolution: "cancel".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("append plan cancellation");
        assert!(
            open_network_policy_from_workflow(replay_events(&store).iter()).is_none(),
            "a cancelled plan never owes a network decision"
        );
    }

    /// `ordered_plan_ids()` must return `(execution, test)` so it lines up
    /// with [`SelectedPlanSet::ordered_ids`] downstream.
    #[test]
    fn ordered_plan_ids_returns_execution_then_test() {
        let outcome = PlanWriteOutcome {
            execution_plan_id: "plan-exec-1".to_string(),
            test_plan_id: "plan-test-1".to_string(),
            plan_id_by_task_id: HashMap::new(),
        };
        let (exec, test) = outcome.ordered_plan_ids();
        assert_eq!(exec, "plan-exec-1");
        assert_eq!(test, "plan-test-1");
    }

    /// `PlanWriteOutcome` must derive `Clone` so observer / audit handlers
    /// can keep a snapshot while the caller continues mutating the
    /// scheduler state.
    #[test]
    fn outcome_is_clone() {
        let task_id = Uuid::new_v4();
        let mut map = HashMap::new();
        map.insert(task_id, "plan-exec-1".to_string());

        let outcome = PlanWriteOutcome {
            execution_plan_id: "plan-exec-1".to_string(),
            test_plan_id: "plan-test-1".to_string(),
            plan_id_by_task_id: map,
        };
        let cloned = outcome.clone();
        assert_eq!(cloned, outcome);
        assert_eq!(
            cloned.plan_id_by_task_id.get(&task_id).map(String::as_str),
            Some("plan-exec-1")
        );
    }

    async fn setup_mcp_server() -> (Arc<LibraMcpServer>, tempfile::TempDir) {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let temp_path = temp_dir.path().to_path_buf();
        let db_path = temp_path.join("libra.db");
        let db = db::create_database(db_path.to_str().expect("utf-8 db path"))
            .await
            .expect("db");
        let storage = Arc::new(LocalStorage::new(temp_path.join("objects")));
        let history = Arc::new(HistoryManager::new(
            storage.clone(),
            temp_path,
            Arc::new(db),
        ));
        (
            Arc::new(LibraMcpServer::new(Some(history), Some(storage))),
            temp_dir,
        )
    }

    fn created_id(result: &rmcp::model::CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|content| content.as_text())
            .and_then(|text| text.text.split("ID:").nth(1))
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .expect("created id")
            .to_string()
    }

    #[tokio::test]
    async fn write_plan_set_persists_execution_and_test_plan_pair() {
        use crate::internal::ai::runtime::contracts::{
            ProjectionVersions, SchedulerMutation, SelectedPlanSet,
        };

        let (server, _temp_dir) = setup_mcp_server().await;
        let actor = ActorRef::agent("phase1-test").expect("actor");
        let intent = server
            .create_intent_impl(
                CreateIntentParams {
                    content: "implement feature and verify it".to_string(),
                    structured_content: None,
                    parent_id: None,
                    parent_ids: None,
                    analysis_context_frame_ids: None,
                    status: Some("active".to_string()),
                    commit_sha: None,
                    reason: None,
                    next_intent_id: None,
                    actor_kind: Some("agent".to_string()),
                    actor_id: Some("phase1-test".to_string()),
                },
                actor,
            )
            .await
            .expect("create intent");
        let intent_id = created_id(&intent);

        let impl_task = {
            let actor = ActorRef::agent("phase1-test").expect("actor");
            GitTask::new(actor, "Edit source", None).expect("task")
        };
        let impl_task_id = impl_task.header().object_id();
        let mut gate_task = {
            let actor = ActorRef::agent("phase1-test").expect("actor");
            GitTask::new(actor, "Run verification", None).expect("task")
        };
        gate_task.add_dependency(impl_task_id);
        let gate_task_id = gate_task.header().object_id();

        let plan = ExecutionPlanSpec {
            intent_spec_id: intent_id.clone(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![
                TaskSpec {
                    step: PlanStep::new("Edit source"),
                    task: impl_task,
                    objective: "Update source".to_string(),
                    kind: TaskKind::Implementation,
                    gate_stage: None,
                    owner_role: Some("coder".to_string()),
                    scope_in: vec!["src/".to_string()],
                    scope_out: vec![],
                    checks: vec![],
                    contract: TaskContract::default(),
                },
                TaskSpec {
                    step: PlanStep::new("Run verification"),
                    task: gate_task,
                    objective: "Verify the change".to_string(),
                    kind: TaskKind::Gate,
                    gate_stage: Some(GateStage::Fast),
                    owner_role: Some("verifier".to_string()),
                    scope_in: vec![],
                    scope_out: vec![],
                    checks: vec![],
                    contract: TaskContract::default(),
                },
            ],
            max_parallel: 1,
            checkpoints: vec![],
        };

        let outcome = write_plan_set(&server, &intent_id, None, None, &plan)
            .await
            .expect("write plan set");

        assert_ne!(outcome.execution_plan_id, outcome.test_plan_id);
        assert_eq!(
            outcome
                .plan_id_by_task_id
                .get(&impl_task_id)
                .map(String::as_str),
            Some(outcome.execution_plan_id.as_str())
        );
        assert_eq!(
            outcome
                .plan_id_by_task_id
                .get(&gate_task_id)
                .map(String::as_str),
            Some(outcome.test_plan_id.as_str())
        );

        let history = server.intent_history_manager.as_ref().expect("history");
        assert_eq!(history.list_objects("plan").await.expect("plans").len(), 2);
        for (object_type, object_id) in [
            ("plan", outcome.execution_plan_id.as_str()),
            ("plan", outcome.test_plan_id.as_str()),
        ] {
            assert!(
                history
                    .get_object_hash(object_type, object_id)
                    .await
                    .expect("history lookup")
                    .is_some(),
                "expected Phase 1 {object_type} id {object_id} to resolve in history",
            );
        }

        let current = dummy_scheduler_state(1);
        let execution_plan_id =
            Uuid::parse_str(&outcome.execution_plan_id).expect("execution plan id");
        let test_plan_id = Uuid::parse_str(&outcome.test_plan_id).expect("test plan id");
        let next = apply_scheduler_mutation(
            &current,
            SchedulerMutation::SelectPlanSet {
                expected: ProjectionVersions {
                    thread: 0,
                    scheduler: 1,
                    live_context_window: 0,
                },
                selected: SelectedPlanSet {
                    execution_plan_id,
                    test_plan_id,
                },
            },
        )
        .expect("selected plan set should apply");
        assert_eq!(next.selected_plan_ids.len(), 2);
        assert_eq!(next.selected_plan_ids[0].plan_id, execution_plan_id);
        assert_eq!(next.selected_plan_ids[0].ordinal, 0);
        assert_eq!(next.selected_plan_ids[1].plan_id, test_plan_id);
        assert_eq!(next.selected_plan_ids[1].ordinal, 1);
        assert_eq!(next.selected_plan_id, Some(execution_plan_id));
    }

    use chrono::Utc;

    use crate::internal::ai::{
        projection::scheduler::SchedulerState,
        runtime::contracts::{ProjectionVersions, SchedulerClearReason, SchedulerMutation},
    };

    fn dummy_scheduler_state(version: i64) -> SchedulerState {
        SchedulerState {
            thread_id: Uuid::new_v4(),
            selected_plan_id: None,
            selected_plan_ids: Vec::new(),
            current_plan_heads: Vec::new(),
            active_task_id: None,
            active_run_id: None,
            live_context_window: Vec::new(),
            metadata: None,
            updated_at: Utc::now(),
            version,
        }
    }

    /// `MarkTaskActive` must set `active_task_id` to the requested task,
    /// pass `run_id` through verbatim (including `None`), bump `version`
    /// by 1, and refresh `updated_at`.
    #[test]
    fn apply_scheduler_mutation_mark_task_active_sets_active_task_and_run() {
        let current = dummy_scheduler_state(7);
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let mutation = SchedulerMutation::MarkTaskActive {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 7,
                live_context_window: 0,
            },
            task_id,
            run_id: Some(run_id),
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("mutation should apply");

        assert_eq!(next.active_task_id, Some(task_id));
        assert_eq!(next.active_run_id, Some(run_id));
        assert_eq!(next.version, 8);
        assert!(next.updated_at >= current.updated_at);
    }

    /// `ClearActiveRun` must zero out `active_run_id` while preserving
    /// `active_task_id` (the task remains the scheduler's focus even
    /// without a live run).
    #[test]
    fn apply_scheduler_mutation_clear_active_run_keeps_task_drops_run() {
        let mut current = dummy_scheduler_state(3);
        current.active_task_id = Some(Uuid::new_v4());
        current.active_run_id = Some(Uuid::new_v4());
        let preserved_task = current.active_task_id;
        let mutation = SchedulerMutation::ClearActiveRun {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 3,
                live_context_window: 0,
            },
            reason: SchedulerClearReason::Completed,
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("mutation should apply");

        assert_eq!(next.active_task_id, preserved_task);
        assert_eq!(next.active_run_id, None);
        assert_eq!(next.version, 4);
    }

    /// Version mismatch must fail-closed with `VersionMismatch` so the
    /// caller can route to a reload-and-retry path instead of silently
    /// writing stale state.
    #[test]
    fn apply_scheduler_mutation_rejects_version_mismatch() {
        let current = dummy_scheduler_state(5);
        let mutation = SchedulerMutation::MarkTaskActive {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 99, // doesn't match current.version == 5
                live_context_window: 0,
            },
            task_id: Uuid::new_v4(),
            run_id: None,
        };

        let error = apply_scheduler_mutation(&current, mutation)
            .expect_err("version mismatch must fail-closed");
        assert_eq!(
            error,
            ApplySchedulerMutationError::VersionMismatch {
                expected: 99,
                actual: 5,
            }
        );
    }

    /// `SeedThread` with a matching bundle must clear active/scheduling
    /// state (no active task / run / plan heads), record the
    /// `intent_id` + `context_snapshot_id` in `metadata.seed_bundle`,
    /// and clear any prior `stale_reason` / `stage` markers since
    /// seeding invalidates them. Unrelated metadata keys must be
    /// preserved.
    #[test]
    fn apply_scheduler_mutation_seed_thread_initialises_clean_state() {
        use serde_json::json;

        use crate::internal::ai::runtime::contracts::Phase0Bundle;

        let mut current = dummy_scheduler_state(1);
        // Pretend the state had leftover task / run / metadata from a
        // previous incarnation — seeding must wipe them all.
        current.active_task_id = Some(Uuid::new_v4());
        current.active_run_id = Some(Uuid::new_v4());
        current.selected_plan_id = Some(Uuid::new_v4());
        current.metadata = Some(json!({
            "stage": "execution",
            "stale_reason": "rebuild_required",
            "previous_marker": "should be preserved"
        }));
        let intent_id = Uuid::new_v4();
        let context_snapshot_id = Uuid::new_v4();
        let mutation = SchedulerMutation::SeedThread {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 1,
                live_context_window: 0,
            },
            bundle: Phase0Bundle {
                thread_id: current.thread_id,
                intent_id,
                context_snapshot_id: Some(context_snapshot_id),
            },
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("seed should apply");

        // Active / scheduling state wiped.
        assert_eq!(next.active_task_id, None);
        assert_eq!(next.active_run_id, None);
        assert_eq!(next.selected_plan_id, None);
        assert!(next.selected_plan_ids.is_empty());
        assert!(next.current_plan_heads.is_empty());

        // Seed bundle recorded; stale_reason / stage cleared; other
        // metadata preserved.
        let metadata = next.metadata.expect("metadata must be set");
        let seed = metadata
            .get("seed_bundle")
            .expect("seed_bundle key should be written");
        assert_eq!(seed["intent_id"], json!(intent_id));
        assert_eq!(seed["context_snapshot_id"], json!(context_snapshot_id));
        assert!(metadata.get("stale_reason").is_none());
        assert!(metadata.get("stage").is_none());
        assert_eq!(
            metadata["previous_marker"],
            json!("should be preserved"),
            "unrelated metadata keys must be preserved"
        );

        // Version bumped.
        assert_eq!(next.version, 2);
    }

    /// `SeedThread` with a bundle targeting a different `thread_id`
    /// must fail-closed with `SeedThreadMismatch` rather than silently
    /// seed across threads. Cross-thread seeding would corrupt
    /// projection state.
    #[test]
    fn apply_scheduler_mutation_seed_thread_rejects_cross_thread_seed() {
        use crate::internal::ai::runtime::contracts::Phase0Bundle;

        let current = dummy_scheduler_state(1);
        let stranger_thread_id = Uuid::new_v4();
        assert_ne!(
            stranger_thread_id, current.thread_id,
            "test sanity: stranger must differ from state thread"
        );
        let mutation = SchedulerMutation::SeedThread {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 1,
                live_context_window: 0,
            },
            bundle: Phase0Bundle {
                thread_id: stranger_thread_id,
                intent_id: Uuid::new_v4(),
                context_snapshot_id: None,
            },
        };

        let error = apply_scheduler_mutation(&current, mutation)
            .expect_err("cross-thread seed must fail-closed");
        assert_eq!(
            error,
            ApplySchedulerMutationError::SeedThreadMismatch {
                bundle_thread_id: stranger_thread_id,
                state_thread_id: current.thread_id,
            }
        );
    }

    /// `SetCurrentPlanHeads` must populate `current_plan_heads` with
    /// execution at ordinal 0 and test at ordinal 1, plus set
    /// `selected_plan_id` to the execution head for legacy single-plan
    /// readers.
    #[test]
    fn apply_scheduler_mutation_set_current_plan_heads_populates_both_heads() {
        let current = dummy_scheduler_state(1);
        let execution_plan_id = Uuid::new_v4();
        let test_plan_id = Uuid::new_v4();
        let mutation = SchedulerMutation::SetCurrentPlanHeads {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 1,
                live_context_window: 0,
            },
            execution_plan_id,
            test_plan_id,
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("should apply");

        assert_eq!(next.current_plan_heads.len(), 2);
        assert_eq!(next.current_plan_heads[0].plan_id, execution_plan_id);
        assert_eq!(next.current_plan_heads[0].ordinal, 0);
        assert_eq!(next.current_plan_heads[1].plan_id, test_plan_id);
        assert_eq!(next.current_plan_heads[1].ordinal, 1);
        assert_eq!(next.selected_plan_id, Some(execution_plan_id));
        assert_eq!(next.version, 2);
    }

    /// `SelectPlanSet` must populate `selected_plan_ids` from
    /// `SelectedPlanSet::ordered_ids` (execution, test) and update
    /// `selected_plan_id` to the execution head.
    #[test]
    fn apply_scheduler_mutation_select_plan_set_populates_ordered_ids() {
        use crate::internal::ai::runtime::contracts::SelectedPlanSet;

        let current = dummy_scheduler_state(2);
        let execution_plan_id = Uuid::new_v4();
        let test_plan_id = Uuid::new_v4();
        let mutation = SchedulerMutation::SelectPlanSet {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 2,
                live_context_window: 0,
            },
            selected: SelectedPlanSet {
                execution_plan_id,
                test_plan_id,
            },
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("should apply");

        assert_eq!(next.selected_plan_ids.len(), 2);
        assert_eq!(next.selected_plan_ids[0].plan_id, execution_plan_id);
        assert_eq!(next.selected_plan_ids[0].ordinal, 0);
        assert_eq!(next.selected_plan_ids[1].plan_id, test_plan_id);
        assert_eq!(next.selected_plan_ids[1].ordinal, 1);
        assert_eq!(next.selected_plan_id, Some(execution_plan_id));
    }

    /// `StartStage` must write a stable lower-snake-case `stage` key into
    /// `metadata` and clear any prior `stale_reason` marker (the stage
    /// transition is itself a freshness signal).
    #[test]
    fn apply_scheduler_mutation_start_stage_writes_stage_metadata() {
        use serde_json::json;

        use crate::internal::ai::runtime::contracts::DagStage;

        let mut current = dummy_scheduler_state(4);
        current.metadata = Some(json!({ "stale_reason": "rebuild_required" }));
        let mutation = SchedulerMutation::StartStage {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 4,
                live_context_window: 0,
            },
            stage: DagStage::Test,
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("should apply");

        let metadata = next.metadata.expect("metadata should be set");
        assert_eq!(metadata["stage"], json!("test"));
        // Prior stale_reason must be cleared on stage transition.
        assert!(
            metadata.get("stale_reason").is_none(),
            "stale_reason should be cleared on stage transition, got {metadata:?}"
        );
    }

    /// `MarkProjectionStale` must persist the reason as a stable
    /// lower-snake-case `stale_reason` key in metadata so a future
    /// `ApplyRebuild` can remove it.
    #[test]
    fn apply_scheduler_mutation_mark_projection_stale_writes_reason() {
        use serde_json::json;

        use crate::internal::ai::runtime::contracts::ProjectionStaleReason;

        let current = dummy_scheduler_state(6);
        let mutation = SchedulerMutation::MarkProjectionStale {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 6,
                live_context_window: 0,
            },
            reason: ProjectionStaleReason::CasConflict,
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("should apply");

        let metadata = next.metadata.expect("metadata should be set");
        assert_eq!(metadata["stale_reason"], json!("cas_conflict"));
    }

    /// `ApplyRebuild` must clear the `stale_reason` marker and record
    /// the freshly materialized `versions` in metadata so downstream
    /// observers can correlate rebuild events with their version
    /// triple.
    #[test]
    fn apply_scheduler_mutation_apply_rebuild_clears_stale_and_records_versions() {
        use serde_json::json;

        use crate::internal::ai::runtime::contracts::{
            MaterializedProjection, ProjectionFreshness,
        };

        let mut current = dummy_scheduler_state(9);
        current.metadata = Some(json!({ "stale_reason": "rebuild_required" }));
        let thread_id = Uuid::new_v4();
        let materialized_versions = ProjectionVersions {
            thread: 5,
            scheduler: 9,
            live_context_window: 7,
        };
        let mutation = SchedulerMutation::ApplyRebuild {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 9,
                live_context_window: 0,
            },
            materialized: MaterializedProjection {
                thread_id,
                versions: materialized_versions,
                freshness: ProjectionFreshness::Fresh,
                summary: json!({}),
            },
        };

        let next = apply_scheduler_mutation(&current, mutation).expect("should apply");

        let metadata = next.metadata.expect("metadata should be set");
        assert!(
            metadata.get("stale_reason").is_none(),
            "stale_reason must be cleared after rebuild"
        );
        let rebuild_versions = metadata
            .get("rebuild_versions")
            .expect("rebuild_versions key should be written");
        assert_eq!(rebuild_versions["thread"], json!(5));
        assert_eq!(rebuild_versions["scheduler"], json!(9));
        assert_eq!(rebuild_versions["live_context_window"], json!(7));
    }
}
