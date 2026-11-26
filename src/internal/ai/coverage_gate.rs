//! Per-turn coverage claim gate (plan-20260713 DR-05c-0, ADR-DR-09/10/16).
//!
//! `agent_coverage_claim` is the WRITE-FRONT idempotence gate: a checkpoint
//! writer reserves the logical turns it is about to cover *before* building
//! objects, and commits the claims (revision insert + claim advance + catalog
//! row) inside the SAME SQLite transaction as the traces ref CAS — so a
//! repeated TurnEnd, a crash retry, or a concurrent writer can never produce
//! a second visible checkpoint for an already-covered turn.
//!
//! Arbitration (ADR-DR-09) is embodied in [`reserve_live_turn_claims`]:
//! equivalent committed content → no-op; committed `incomplete` upgraded by
//! new `complete` content → revision advance; committed `complete` vs a
//! different `complete` digest → `conflicted` (doctor's job, never silent
//! overwrite); an unexpired foreign reservation → skip (someone else is
//! writing this turn); an expired one → fenced takeover. Every mutation is a
//! conditional write checked via `rows_affected == 1` — losers re-read, they
//! never assume.
//!
//! Failure policy (ADR-DR-10): any DB error here fails the checkpoint write
//! *closed* — the caller must not append to `refs/libra/traces` without a
//! reservation, and a commit-time fence mismatch rolls the whole final
//! transaction back (ref update included). A transient SQLite writer-lock
//! race is reported as an in-flight skip so the caller emits the same
//! replayable diagnostic as an explicit foreign reservation.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};

#[cfg(test)]
pub(crate) use crate::internal::ai::capture::catalog::CaptureImportSessionLifecycleState as ImportSessionLifecycleState;
use crate::{
    internal::{
        ai::{
            capture::{
                catalog::{CaptureCatalogStore, CaptureImportSessionCommit},
                checkpoint::{
                    CheckpointCatalogMutation, NativeVersionedReasoningCoveragePlan,
                    ReasoningCoveragePlan, ReasoningIdentityReadContext, ReasoningTurnIdentity,
                    StoreBoundNativeVersionedPermit,
                },
            },
            capture_scope::{
                CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
                authorize_final_capture_commit,
            },
            history::{CheckpointScope, TracesCommitCtx, TracesTxnExtra},
            observed_agents::{
                COVERAGE_SCHEMA_VERSION, Completeness, CoverageSchemaVersion, NormalizedTurn,
                encode_coverage_turn_for_schema, redact_turns_with_report, select_coverage_schema,
            },
        },
        metadata::{
            MetadataKv, MetadataScope, MetadataValueType, ReasoningIdentityKind,
            ReasoningIdentityRecord, ReasoningIdentityRecordBody, ReasoningIdentityRecordContext,
            ReasoningIdentityRecordV2, ReasoningIdentitySessionMap, ReasoningIdentitySessionMapV2,
            validate_reasoning_identity_private_map_keys,
        },
    },
    storage::capture_store::{
        AgentCoverageClaimRecord, AgentCoverageConflictRecord, AgentCoverageRevisionRecord,
        CaptureCompleteness, CaptureCoverageClaimState, CaptureSourceChannel, CaptureStore,
    },
};

/// Live reservation lease length. Generous relative to a single hook write
/// (sub-second) so takeover only fires on genuinely dead writers.
const LIVE_LEASE_MS: i64 = 60_000;

/// Lease sentinel for a `reserved_live` claim whose turns belong to a durable
/// pending terminal artifact (ADR-ACF-09a "Write trigger"). The artifact, not
/// a process, owns these claims: its replay commits them under the original
/// owner/fence, and only that commit or an explicit session erase releases
/// them. No live, export, or import writer may take them over on lease expiry.
pub(crate) const ARTIFACT_RETAINED_LEASE_MS: i64 = i64::MAX;

/// A failed managed capture may only spend this short, paired window releasing
/// an existing owned reservation. This is recovery evidence cleanup, never an
/// allowance to begin a new coverage reservation after the foreground budget.
const COVERAGE_ABANDONMENT_RECOVERY_GRACE: Duration = Duration::from_millis(250);

pub(crate) const REASONING_IDENTITY_CONFLICT_REASON: &str = "reasoning_identity_changed";

// Batch-delay injection is useful for proving that a historical import checks
// its total deadline between SQLite transactions. It must not be a hook
// environment knob: external integration binaries compile this module without
// `cfg(test)`, so only in-process unit tests can opt into the delay.
#[cfg(test)]
tokio::task_local! {
    static TEST_IMPORT_RESERVATION_BATCH_DELAY: Option<std::time::Duration>;
    static TEST_LIVE_RESERVATION_TURN_DELAY: Option<std::time::Duration>;
}

#[cfg(test)]
pub(crate) async fn with_import_reservation_batch_delay<F>(
    delay: std::time::Duration,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_IMPORT_RESERVATION_BATCH_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
fn import_reservation_batch_test_delay() -> Option<std::time::Duration> {
    TEST_IMPORT_RESERVATION_BATCH_DELAY
        .try_with(|delay| *delay)
        .ok()
        .flatten()
}

/// Test-only delay after each live reservation decision. It exercises the
/// rollback path when a single all-or-nothing live transaction outlives its
/// capture deadline; production has no delay/configuration seam here.
#[cfg(test)]
pub(crate) async fn with_live_reservation_turn_delay<F>(
    delay: std::time::Duration,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_LIVE_RESERVATION_TURN_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
fn live_reservation_turn_test_delay() -> Option<std::time::Duration> {
    TEST_LIVE_RESERVATION_TURN_DELAY
        .try_with(|delay| *delay)
        .ok()
        .flatten()
}

/// One reserved turn: the writer holds `(owner, fence_token)` and must present
/// both at commit time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservedTurnClaim {
    pub logical_turn_key: String,
    pub coverage_digest: String,
    pub completeness: Completeness,
    pub fence_token: i64,
    pub next_revision: i64,
}

/// The claim's version travels beside the frozen v1 reservation shape. A
/// logical turn key may be present once in each version; callers must never
/// reconstruct this version from the current transcript during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedReservedTurnClaim {
    pub coverage_schema_version: CoverageSchemaVersion,
    pub claim: ReservedTurnClaim,
}

impl VersionedReservedTurnClaim {
    pub fn new(version: i64, claim: ReservedTurnClaim) -> Result<Self> {
        Ok(Self {
            coverage_schema_version: CoverageSchemaVersion::try_from(version)?,
            claim,
        })
    }
}

/// An ordinal-aligned version sidecar for the unchanged `LiveClaimCommitPlan`
/// literal. COV2-03 can use this for marker/ref fences; COV2-04 can encode it
/// into pending without guessing a version from a repeated logical key.
#[derive(Clone)]
pub struct VersionedLiveClaimCommitPlan {
    ordinary: LiveClaimCommitPlan,
    versions: Vec<CoverageSchemaVersion>,
}

impl VersionedLiveClaimCommitPlan {
    pub fn new(
        ordinary: LiveClaimCommitPlan,
        versioned: &[VersionedReservedTurnClaim],
    ) -> Result<Self> {
        ensure!(
            ordinary.claims.len() == versioned.len(),
            "versioned coverage claim plan length changed"
        );
        let mut seen = std::collections::BTreeSet::new();
        let mut versions = Vec::with_capacity(versioned.len());
        for (claim, bound) in ordinary.claims.iter().zip(versioned) {
            ensure!(
                claim == &bound.claim
                    && seen.insert((
                        bound.coverage_schema_version.as_i64(),
                        claim.logical_turn_key.as_str()
                    )),
                "versioned coverage claim plan binding changed"
            );
            versions.push(bound.coverage_schema_version);
        }
        Ok(Self { ordinary, versions })
    }

    pub fn ordinary(&self) -> &LiveClaimCommitPlan {
        &self.ordinary
    }

    pub fn claims(&self) -> impl Iterator<Item = (CoverageSchemaVersion, &ReservedTurnClaim)> {
        self.versions.iter().copied().zip(&self.ordinary.claims)
    }
}

/// Verify retained live reservations before an authenticated recovery artifact
/// takes responsibility for them. Lease expiry alone does not revoke a token;
/// takeover or abandonment advances its fence and must reject this writer.
pub(crate) async fn verify_reserved_live_claims_with_conn<C: ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    claims: &[ReservedTurnClaim],
) -> Result<()> {
    scope.assert_workspace_fence_live(conn).await?;
    for claim in claims {
        let owned = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_coverage_claim
             WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ?
               AND state = 'reserved_live' AND owner = ? AND fence_token = ?
               AND revision = ? LIMIT 1",
                [
                    session_id.into(),
                    claim.logical_turn_key.clone().into(),
                    COVERAGE_SCHEMA_VERSION.into(),
                    owner.into(),
                    claim.fence_token.into(),
                    (claim.next_revision - 1).into(),
                ],
            ))
            .await
            .context("cannot verify retained capture coverage claims; run `libra agent doctor`")?;
        if owned.is_none() {
            bail!(
                "capture recovery coverage ownership changed; retry through the current provider session"
            );
        }
    }
    Ok(())
}

/// Read back the exact sealed version-keyed reservation tuple in the
/// caller's connection (normally its pending-artifact SQLite transaction).
/// Lease expiry alone does not revoke ownership: only a changed owner, fence,
/// revision, digest, completeness, or state does. This helper never updates a
/// claim or opens an independent pool connection.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn verify_versioned_reserved_live_claims_with_conn<C: ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
    plan: &VersionedLiveClaimCommitPlan,
) -> Result<()> {
    let ordinary = plan.ordinary();
    ensure!(
        ordinary.capture_scope.as_ref() == Some(scope),
        "capture recovery coverage scope changed; retry through the current provider session"
    );
    ensure!(
        !ordinary.claims.is_empty()
            && ordinary.claims.len()
                <= crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES,
        "capture recovery coverage claim count is invalid; run `libra agent doctor`"
    );
    scope.assert_workspace_fence_live(conn).await?;
    for (version, claim) in plan.claims() {
        let previous_revision = claim
            .next_revision
            .checked_sub(1)
            .context("capture recovery coverage revision is invalid; run `libra agent doctor`")?;
        ensure!(
            previous_revision >= 0 && claim.fence_token > 0,
            "capture recovery coverage fence is invalid; run `libra agent doctor`"
        );
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT coverage_digest, completeness, owner, fence_token, revision, state
                 FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?
                   AND coverage_schema_version = ? LIMIT 1",
                [
                    ordinary.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                    version.as_i64().into(),
                ],
            ))
            .await
            .context("cannot verify versioned capture coverage claims; run `libra agent doctor`")?;
        let Some(row) = row else {
            bail!(
                "capture recovery coverage ownership changed; retry through the current provider session"
            );
        };
        let digest: String = row
            .try_get_by("coverage_digest")
            .context("cannot decode versioned capture coverage digest; run `libra agent doctor`")?;
        let completeness: String = row.try_get_by("completeness").context(
            "cannot decode versioned capture coverage completeness; run `libra agent doctor`",
        )?;
        let owner: Option<String> = row
            .try_get_by("owner")
            .context("cannot decode versioned capture coverage owner; run `libra agent doctor`")?;
        let fence_token: Option<i64> = row
            .try_get_by("fence_token")
            .context("cannot decode versioned capture coverage fence; run `libra agent doctor`")?;
        let revision: i64 = row.try_get_by("revision").context(
            "cannot decode versioned capture coverage revision; run `libra agent doctor`",
        )?;
        let state: String = row
            .try_get_by("state")
            .context("cannot decode versioned capture coverage state; run `libra agent doctor`")?;
        ensure!(
            digest == claim.coverage_digest
                && completeness == claim.completeness.as_db_str()
                && owner.as_deref() == Some(ordinary.owner.as_str())
                && fence_token == Some(claim.fence_token)
                && revision == previous_revision
                && state == "reserved_live",
            "capture recovery coverage ownership changed; retry through the current provider session"
        );
    }
    Ok(())
}

/// Retain the sealed version-keyed reservations in the caller-owned artifact
/// transaction. The caller must first verify the same plan on this transaction
/// and roll the whole transaction back on any error: an earlier version may
/// already have been updated when a later conditional UPDATE or SQL fault fails.
/// This helper neither opens a second connection nor commits the transaction.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn retain_versioned_reserved_live_claims_for_artifact_with_conn(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
    plan: &VersionedLiveClaimCommitPlan,
) -> Result<()> {
    let ordinary = plan.ordinary();
    ensure!(
        ordinary.capture_scope.as_ref() == Some(scope)
            && reservation_state(ordinary.source_channel)? == "reserved_live",
        "capture recovery coverage scope or channel changed; run `libra agent doctor`"
    );
    ensure!(
        !ordinary.claims.is_empty()
            && ordinary.claims.len()
                <= crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES,
        "capture recovery coverage claim count is invalid; run `libra agent doctor`"
    );
    scope.assert_workspace_fence_live(txn).await?;
    for (version, claim) in plan.claims() {
        let previous_revision = claim
            .next_revision
            .checked_sub(1)
            .context("capture recovery coverage revision is invalid; run `libra agent doctor`")?;
        ensure!(
            previous_revision >= 0 && claim.fence_token > 0,
            "capture recovery coverage fence is invalid; run `libra agent doctor`"
        );
        let retained = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_coverage_claim SET lease_expires_at = ?
                 WHERE session_id = ? AND logical_turn_key = ?
                   AND coverage_schema_version = ? AND coverage_digest = ?
                   AND completeness = ? AND state = 'reserved_live'
                   AND owner = ? AND fence_token = ? AND revision = ?",
                [
                    ARTIFACT_RETAINED_LEASE_MS.into(),
                    ordinary.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                    version.as_i64().into(),
                    claim.coverage_digest.clone().into(),
                    claim.completeness.as_db_str().into(),
                    ordinary.owner.clone().into(),
                    claim.fence_token.into(),
                    previous_revision.into(),
                ],
            ))
            .await
            .context("cannot retain versioned capture coverage claims; run `libra agent doctor`")?;
        ensure!(
            retained.rows_affected() == 1,
            "capture recovery coverage ownership changed; retry through the current provider session"
        );
    }
    Ok(())
}

/// Hand verified live reservations to the durable artifact that is being
/// persisted in this same writer transaction. The same owner/fence/revision
/// guards as [`verify_reserved_live_claims_with_conn`] apply; the lease
/// becomes [`ARTIFACT_RETAINED_LEASE_MS`] so an ordinary lease expiry can no
/// longer revoke the artifact's replay fence.
pub(crate) async fn retain_reserved_live_claims_for_artifact_with_conn<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    owner: &str,
    claims: &[ReservedTurnClaim],
) -> Result<()> {
    for claim in claims {
        let retained = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_coverage_claim SET lease_expires_at = ?
                 WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ?
                   AND state = 'reserved_live' AND owner = ? AND fence_token = ?
                   AND revision = ?",
                [
                    ARTIFACT_RETAINED_LEASE_MS.into(),
                    session_id.into(),
                    claim.logical_turn_key.clone().into(),
                    COVERAGE_SCHEMA_VERSION.into(),
                    owner.into(),
                    claim.fence_token.into(),
                    (claim.next_revision - 1).into(),
                ],
            ))
            .await
            .context("cannot retain capture coverage claims; run `libra agent doctor`")?;
        if retained.rows_affected() != 1 {
            bail!(
                "capture recovery coverage ownership changed; retry through the current provider session"
            );
        }
    }
    Ok(())
}

/// Outcome of a live reservation pass over one snapshot's normalized turns.
#[derive(Debug, Default)]
pub struct LiveReservationOutcome {
    pub reserved: Vec<ReservedTurnClaim>,
    /// Turns already covered by equivalent-or-better committed content.
    pub skipped_covered: usize,
    /// Turns currently reserved by another live writer (unexpired lease).
    pub skipped_inflight: usize,
    /// Subset of `skipped_inflight` held by a durable pending terminal
    /// artifact rather than a running writer. They stay "in flight" for
    /// export/import retry decisions, but no lease expiry will release them.
    pub skipped_retained: usize,
    /// Turns whose committed `complete` content differs from this snapshot's
    /// `complete` content — flagged `conflicted` for doctor, never rewritten.
    pub conflicted: usize,
}

/// A reservation result whose claims retain their individual DB identity.
/// The v1 entry points continue returning `LiveReservationOutcome` unchanged.
#[derive(Debug, Default)]
pub struct VersionedReservationOutcome {
    pub reserved: Vec<VersionedReservedTurnClaim>,
    pub skipped_covered: usize,
    pub skipped_inflight: usize,
    pub skipped_retained: usize,
    pub conflicted: usize,
}

impl LiveReservationOutcome {
    /// Nothing to write: every turn is covered / in flight / conflicted.
    pub fn is_noop(&self) -> bool {
        self.reserved.is_empty()
    }

    /// Codex M3 R2 P1-2: this pass reserved nothing to append, yet at least
    /// one turn is held by another LIVE writer (unexpired lease). The export
    /// job must then be released `dirty` (retryable) rather than advanced
    /// clean — if that writer crashes, its claim lease expires and only a
    /// dirty job lets a later idle recapture the transcript. A genuine
    /// all-covered no-op (nothing in flight) is NOT this case.
    pub fn is_inflight_only_skip(&self) -> bool {
        self.reserved.is_empty() && self.skipped_inflight > 0
    }
}

/// Carries partial reasoning separately from the incumbent public outcome.
/// Legacy callers cannot discard this state and mislabel it CoveredNoop.
#[derive(Default)]
pub(crate) struct ReasoningReservationOutcome {
    pub(crate) ordinary: LiveReservationOutcome,
    pub(crate) versioned_reserved: Vec<VersionedReservedTurnClaim>,
    pub(crate) covered: Vec<CommittedTurnClaimSnapshot>,
    pub(crate) skipped_incomplete_reasoning: usize,
}

/// Gate-owned observation of an actual committed row. Callers cannot mint
/// replay expectations from transcript data or a coverage count.
pub(crate) struct CommittedTurnClaimSnapshot {
    session_id: String,
    version: CoverageSchemaVersion,
    logical_turn_key: String,
    coverage_digest: String,
    completeness: Completeness,
    revision: i64,
    checkpoint_id: String,
    fence_token: i64,
    requires_native_replay: bool,
}

impl CommittedTurnClaimSnapshot {
    pub(crate) fn checkpoint_id(&self) -> &str {
        &self.checkpoint_id
    }

    pub(crate) fn version(&self) -> CoverageSchemaVersion {
        self.version
    }

    pub(crate) fn replay_fence(&self) -> crate::internal::ai::traces::TracesCoverageFence<'_> {
        crate::internal::ai::traces::TracesCoverageFence {
            coverage_schema_version: self.version,
            logical_turn_key: &self.logical_turn_key,
            coverage_digest: &self.coverage_digest,
            completeness: self.completeness,
            next_revision: self.revision,
            // Replay compares committed membership, never reservation authority.
            owner: "committed-replay",
            fence_token: self.fence_token,
            reservation_state: "reserved_live",
        }
    }

    pub(crate) async fn verify_current_with_conn(
        &self,
        conn: &impl ConnectionTrait,
        session_id: &str,
    ) -> Result<()> {
        ensure!(
            !self.requires_native_replay,
            "native covered replay requires its authenticated source companion; run libra agent doctor"
        );
        ensure!(
            self.session_id == session_id,
            "covered replay session changed; run libra agent doctor"
        );
        let current = read_claim(conn, session_id, &self.logical_turn_key, self.version)
            .await?
            .context("covered replay claim is missing; run libra agent doctor")?;
        ensure!(
            current.state == "catalog_committed"
                && current.coverage_digest == self.coverage_digest
                && current.completeness == self.completeness.as_db_str()
                && current.revision == self.revision
                && current.checkpoint_id.as_deref() == Some(self.checkpoint_id.as_str())
                && current.fence_token == Some(self.fence_token),
            "covered replay claim was superseded; run libra agent doctor"
        );
        Ok(())
    }

    fn from_committed(
        session_id: &str,
        turn_key: &str,
        version: CoverageSchemaVersion,
        claim: ExistingClaim,
        requires_native_replay: bool,
    ) -> Result<Self> {
        ensure!(
            claim.state == "catalog_committed",
            "covered replay row is not committed; run libra agent doctor"
        );
        let completeness = match claim.completeness.as_str() {
            "complete" => Completeness::Complete,
            "incomplete" => Completeness::Incomplete,
            _ => bail!("covered replay completeness is invalid; run libra agent doctor"),
        };
        let checkpoint_id = claim
            .checkpoint_id
            .filter(|id| !id.is_empty())
            .context("covered replay checkpoint is missing; run libra agent doctor")?;
        let fence_token = claim
            .fence_token
            .filter(|token| *token > 0)
            .context("covered replay fence is missing; run libra agent doctor")?;
        ensure!(
            claim.revision > 0,
            "covered replay revision is invalid; run libra agent doctor"
        );
        Ok(Self {
            session_id: session_id.to_owned(),
            version,
            logical_turn_key: turn_key.to_owned(),
            coverage_digest: claim.coverage_digest,
            completeness,
            revision: claim.revision,
            checkpoint_id,
            fence_token,
            requires_native_replay,
        })
    }
}

impl ReasoningReservationOutcome {
    fn into_ordinary(self) -> Result<LiveReservationOutcome> {
        if self.skipped_incomplete_reasoning != 0 {
            bail!("reasoning coverage is partial; run libra agent doctor before retrying capture");
        }
        Ok(self.ordinary)
    }

    // COV2-11 wires the versioned writer; COV2-02 exercises this seam in
    // isolated claim tests while production callers remain on v1.
    #[cfg_attr(not(test), allow(dead_code))]
    fn into_versioned(self) -> Result<VersionedReservationOutcome> {
        if self.skipped_incomplete_reasoning != 0 {
            bail!("reasoning coverage is partial; run libra agent doctor before retrying capture");
        }
        ensure!(
            self.versioned_reserved.len() == self.ordinary.reserved.len(),
            "versioned coverage reservation lost a claim binding"
        );
        Ok(VersionedReservationOutcome {
            reserved: self.versioned_reserved,
            skipped_covered: self.ordinary.skipped_covered,
            skipped_inflight: self.ordinary.skipped_inflight,
            skipped_retained: self.ordinary.skipped_retained,
            conflicted: self.ordinary.conflicted,
        })
    }
}

/// A read context does not supply a comparable native candidate. Keeping
/// those capabilities separate lets candidate-free legacy turns retain
/// their old coverage semantics without accepting a retained native turn.
#[derive(Clone, Default)]
pub(crate) struct ReasoningCoverageInput<'a> {
    candidate: Option<&'a ReasoningCoveragePlan>,
    versioned_candidate: Option<&'a NativeVersionedReasoningCoveragePlan>,
    reader: Option<ReasoningIdentityReadContext>,
    ordinary_retention_observations: bool,
}

impl<'a> ReasoningCoverageInput<'a> {
    pub(crate) fn native(candidate: &'a ReasoningCoveragePlan) -> Self {
        Self {
            candidate: Some(candidate),
            versioned_candidate: None,
            reader: Some(candidate.read_context()),
            ordinary_retention_observations: false,
        }
    }

    #[cfg_attr(not(all(test, unix)), allow(dead_code))]
    pub(crate) fn native_versioned(candidate: &'a NativeVersionedReasoningCoveragePlan) -> Self {
        Self {
            candidate: None,
            versioned_candidate: Some(candidate),
            reader: Some(candidate.read_context()),
            ordinary_retention_observations: false,
        }
    }

    pub(crate) fn ordinary(reader: &ReasoningIdentityReadContext) -> Self {
        Self {
            candidate: None,
            versioned_candidate: None,
            reader: Some(reader.clone()),
            ordinary_retention_observations: false,
        }
    }
    pub(crate) fn with_ordinary_retention_observations(mut self) -> Self {
        self.ordinary_retention_observations = true;
        self
    }
}

#[derive(Default)]
struct VerifiedReasoningMaps {
    v1: Option<ReasoningIdentitySessionMap>,
    v2: Option<ReasoningIdentitySessionMapV2>,
    v1_encoded_bytes: usize,
    v2_encoded_bytes: usize,
}

impl VerifiedReasoningMaps {
    fn entries(&self) -> Result<usize> {
        self.v1
            .as_ref()
            .map_or(0, |map| map.records().len())
            .checked_add(self.v2.as_ref().map_or(0, |map| map.records().len()))
            .context("private reasoning identity exceeds its safe capacity")
    }

    fn encoded_bytes(&self) -> Result<usize> {
        self.v1_encoded_bytes
            .checked_add(self.v2_encoded_bytes)
            .context("private reasoning identity exceeds its safe capacity")
    }
}

enum StoredReasoningMap {
    LegacyAbsent,
    Invalid,
    Verified(Box<VerifiedReasoningMaps>),
}

fn validate_reasoning_map_aggregate_capacity(entries: usize, encoded_bytes: usize) -> Result<()> {
    if entries > crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES
        || encoded_bytes > crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES
    {
        return Err(crate::internal::metadata::ReasoningIdentityMapCapacity.into());
    }
    Ok(())
}

/// Enumerate the complete bounded key set without materializing private map
/// text. Only after key, type and aggregate size validation do we fetch and
/// authenticate either value in the same transaction.
async fn read_reasoning_map(
    conn: &impl ConnectionTrait,
    scope: &CaptureScope,
    session_id: &str,
    reader: Option<&ReasoningIdentityReadContext>,
) -> Result<StoredReasoningMap> {
    // The map is a repository `metadata_kv` row; a capture-only connection
    // reads it from `libra.db`.
    if let Some(peer) = crate::internal::metadata::repository_metadata_peer(conn).await? {
        return Box::pin(read_reasoning_map(&peer, scope, session_id, reader)).await;
    }
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT CASE WHEN typeof(key) = 'text' AND length(CAST(key AS BLOB)) <= ?
                         THEN key ELSE NULL END AS bounded_key,
                    typeof(value) AS sqlite_value_type,
                    CASE WHEN typeof(value_type) = 'text'
                              AND length(CAST(value_type AS BLOB)) <= 16
                         THEN value_type ELSE NULL END AS bounded_value_type,
                    length(CAST(value AS BLOB)) AS value_bytes
             FROM metadata_kv WHERE scope = ? AND target = ? ORDER BY key LIMIT 3",
            [
                (crate::internal::metadata::MAX_KEY_LEN as i64).into(),
                MetadataScope::AgentReasoningIdentity.as_str().into(),
                session_id.into(),
            ],
        ))
        .await
        .context("cannot read private reasoning identity; check the repository database or start a new capture session")?;
    if rows.is_empty() {
        return Ok(StoredReasoningMap::LegacyAbsent);
    }
    if rows.len() > 2 {
        return Ok(StoredReasoningMap::Invalid);
    }
    let mut keys = std::collections::BTreeMap::new();
    let mut encoded_bytes = 0usize;
    for row in &rows {
        let Some(key) = row.try_get_by::<Option<String>, _>("bounded_key")? else {
            return Ok(StoredReasoningMap::Invalid);
        };
        if row.try_get_by::<String, _>("sqlite_value_type")? != "text"
            || row
                .try_get_by::<Option<String>, _>("bounded_value_type")?
                .as_deref()
                != Some("text")
        {
            return Ok(StoredReasoningMap::Invalid);
        }
        let Some(value_bytes) = row.try_get_by::<Option<i64>, _>("value_bytes")? else {
            return Ok(StoredReasoningMap::Invalid);
        };
        let Ok(value_bytes) = usize::try_from(value_bytes) else {
            return Ok(StoredReasoningMap::Invalid);
        };
        let Some(total) = encoded_bytes.checked_add(value_bytes) else {
            return Ok(StoredReasoningMap::Invalid);
        };
        encoded_bytes = total;
        if keys.insert(key, value_bytes).is_some() {
            return Ok(StoredReasoningMap::Invalid);
        }
    }
    if validate_reasoning_identity_private_map_keys(keys.keys().map(String::as_str)).is_err()
        || validate_reasoning_map_aggregate_capacity(0, encoded_bytes).is_err()
    {
        return Ok(StoredReasoningMap::Invalid);
    }
    let Some(reader) = reader else {
        return Ok(StoredReasoningMap::Invalid);
    };
    let values = conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT key,
                    CASE WHEN typeof(value) = 'text' AND length(CAST(value AS BLOB)) <= ?
                         THEN value ELSE NULL END AS bounded_value
             FROM metadata_kv WHERE scope = ? AND target = ?
               AND key IN (?, ?) ORDER BY key LIMIT 3",
            [
                (crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES as i64).into(),
                MetadataScope::AgentReasoningIdentity.as_str().into(),
                session_id.into(),
                ReasoningIdentitySessionMap::KEY.into(),
                ReasoningIdentitySessionMapV2::KEY.into(),
            ],
        ))
        .await
        .context("cannot load private reasoning identity; check the repository database or start a new capture session")?;
    if values.len() != keys.len() {
        return Ok(StoredReasoningMap::Invalid);
    }
    let mut maps = VerifiedReasoningMaps::default();
    for row in values {
        let key = row.try_get_by::<String, _>("key")?;
        let Some(expected_bytes) = keys.get(&key) else {
            return Ok(StoredReasoningMap::Invalid);
        };
        let Some(text) = row.try_get_by::<Option<String>, _>("bounded_value")? else {
            return Ok(StoredReasoningMap::Invalid);
        };
        if text.len() != *expected_bytes {
            return Ok(StoredReasoningMap::Invalid);
        }
        match key.as_str() {
            ReasoningIdentitySessionMap::KEY => {
                let Ok(map) = ReasoningIdentitySessionMap::decode(&text) else {
                    return Ok(StoredReasoningMap::Invalid);
                };
                if map.validate_owner(&scope.repo_id, session_id).is_err() {
                    return Ok(StoredReasoningMap::Invalid);
                }
                let bytes = ReasoningIdentitySessionMap::authentication_body(
                    &scope.repo_id,
                    session_id,
                    map.records(),
                )?;
                if let Err(error) = reader
                    .authenticate_map_for_schema_version(conn, 1, &bytes, Some(map.mac()))
                    .await
                {
                    if error.is::<crate::internal::ai::capture::key::ReasoningIdentityMacMismatch>()
                    {
                        return Ok(StoredReasoningMap::Invalid);
                    }
                    return Err(error);
                }
                maps.v1_encoded_bytes = text.len();
                maps.v1 = Some(map);
            }
            ReasoningIdentitySessionMapV2::KEY => {
                let Ok(map) = ReasoningIdentitySessionMapV2::decode(&text) else {
                    return Ok(StoredReasoningMap::Invalid);
                };
                if map.validate_owner(&scope.repo_id, session_id).is_err() {
                    return Ok(StoredReasoningMap::Invalid);
                }
                let bytes = ReasoningIdentitySessionMapV2::authentication_body(
                    &scope.repo_id,
                    session_id,
                    map.records(),
                )?;
                if let Err(error) = reader
                    .authenticate_map_for_schema_version(conn, 2, &bytes, Some(map.mac()))
                    .await
                {
                    if error.is::<crate::internal::ai::capture::key::ReasoningIdentityMacMismatch>()
                    {
                        return Ok(StoredReasoningMap::Invalid);
                    }
                    return Err(error);
                }
                maps.v2_encoded_bytes = text.len();
                maps.v2 = Some(map);
            }
            _ => return Ok(StoredReasoningMap::Invalid),
        }
    }
    if validate_reasoning_map_aggregate_capacity(maps.entries()?, maps.encoded_bytes()?).is_err() {
        return Ok(StoredReasoningMap::Invalid);
    }
    Ok(StoredReasoningMap::Verified(Box::new(maps)))
}

enum ReasoningComparison {
    Ordinary,
    Same,
    Changed,
    Partial,
}

enum StoredReasoningRecord<'a> {
    V1(&'a ReasoningIdentityRecord),
    V2(&'a ReasoningIdentityRecordV2),
}

impl StoredReasoningRecord<'_> {
    fn body(&self) -> &ReasoningIdentityRecordBody {
        match self {
            Self::V1(record) => record.body(),
            Self::V2(record) => record.body(),
        }
    }

    fn mac(&self) -> &str {
        match self {
            Self::V1(record) => record.mac(),
            Self::V2(record) => record.mac(),
        }
    }

    fn validate_context(&self, expected: &ReasoningIdentityRecordContext) -> Result<()> {
        match self {
            Self::V1(record) => record.validate_context(expected),
            Self::V2(record) => record.validate_context(expected),
        }
    }

    fn authentication_body(&self) -> Result<Vec<u8>> {
        match self {
            Self::V1(record) => ReasoningIdentityRecord::authentication_body(record.body()),
            Self::V2(record) => ReasoningIdentityRecordV2::authentication_body(record.body()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn compare_reasoning_identity(
    conn: &impl ConnectionTrait,
    scope: &CaptureScope,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: CoverageSchemaVersion,
    existing: &ExistingClaim,
    incoming: Option<&ReasoningTurnIdentity>,
    map: &StoredReasoningMap,
    reader: Option<&ReasoningIdentityReadContext>,
) -> Result<ReasoningComparison> {
    let record = match map {
        StoredReasoningMap::LegacyAbsent => None,
        StoredReasoningMap::Invalid => return Ok(ReasoningComparison::Partial),
        StoredReasoningMap::Verified(maps) => {
            let slot = match incoming {
                Some(incoming) => incoming.slot().to_owned(),
                None => {
                    let Some(reader) = reader else {
                        return Ok(ReasoningComparison::Partial);
                    };
                    match coverage_schema_version {
                        CoverageSchemaVersion::V1 => {
                            reader.turn_slot(conn, logical_turn_key).await?
                        }
                        CoverageSchemaVersion::V2 => {
                            reader
                                .turn_slot_for_schema_version(conn, 2, logical_turn_key)
                                .await?
                        }
                    }
                }
            };
            match coverage_schema_version {
                CoverageSchemaVersion::V1 => maps
                    .v1
                    .as_ref()
                    .and_then(|map| map.records().get(&slot))
                    .map(StoredReasoningRecord::V1),
                CoverageSchemaVersion::V2 => maps
                    .v2
                    .as_ref()
                    .and_then(|map| map.records().get(&slot))
                    .map(StoredReasoningRecord::V2),
            }
        }
    };
    let Some(record) = record else {
        return Ok(
            if incoming.is_some_and(|item| item.kind() == ReasoningIdentityKind::Nonempty)
                && existing.completeness != "incomplete"
            {
                ReasoningComparison::Partial
            } else {
                ReasoningComparison::Ordinary
            },
        );
    };
    let (Some(incoming), Some(reader), Some(fence), Some(checkpoint)) = (
        incoming,
        reader,
        existing.fence_token,
        &existing.checkpoint_id,
    ) else {
        return Ok(ReasoningComparison::Partial);
    };
    let expected = ReasoningIdentityRecordContext {
        repository_id: scope.repo_id.clone(),
        session_id: session_id.to_owned(),
        logical_turn_key: incoming.slot().to_owned(),
        coverage_schema_version: coverage_schema_version.as_i64(),
        claim_revision: existing.revision,
        claim_fence_token: fence,
        checkpoint_id: checkpoint.clone(),
    };
    if record.validate_context(&expected).is_err() {
        return Ok(ReasoningComparison::Partial);
    }
    let catalog = conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT 1 FROM agent_checkpoint WHERE checkpoint_id = ? AND session_id = ? AND scope = 'committed' LIMIT 1",
        [checkpoint.clone().into(), session_id.into()],
    )).await.context("cannot verify private reasoning checkpoint association; run libra agent doctor")?;
    if catalog.is_none() {
        return Ok(ReasoningComparison::Partial);
    }
    let body = record.authentication_body()?;
    let authentication = match coverage_schema_version {
        CoverageSchemaVersion::V1 => {
            reader
                .authenticate_record(conn, &body, Some(record.mac()))
                .await
        }
        CoverageSchemaVersion::V2 => {
            reader
                .authenticate_record_for_schema_version(conn, 2, &body, Some(record.mac()))
                .await
        }
    };
    if let Err(error) = authentication {
        if error.is::<crate::internal::ai::capture::key::ReasoningIdentityMacMismatch>() {
            return Ok(ReasoningComparison::Partial);
        }
        return Err(error);
    }
    Ok(
        if record.body().kind == incoming.kind()
            && record.body().descriptor_mac == incoming.descriptor_mac()
        {
            ReasoningComparison::Same
        } else {
            ReasoningComparison::Changed
        },
    )
}

struct ExistingClaim {
    coverage_digest: String,
    completeness: String,
    revision: i64,
    state: String,
    lease_expires_at: Option<i64>,
    fence_token: Option<i64>,
    checkpoint_id: Option<String>,
}

async fn read_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: CoverageSchemaVersion,
) -> Result<Option<ExistingClaim>> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT coverage_digest, completeness, revision, state,
                    lease_expires_at, fence_token, checkpoint_id
             FROM agent_coverage_claim
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ?",
            [
                session_id.into(),
                logical_turn_key.into(),
                coverage_schema_version.as_i64().into(),
            ],
        ))
        .await
        .context("query agent_coverage_claim")?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(ExistingClaim {
        coverage_digest: row.try_get_by("coverage_digest")?,
        completeness: row.try_get_by("completeness")?,
        revision: row.try_get_by("revision")?,
        state: row.try_get_by("state")?,
        lease_expires_at: row.try_get_by("lease_expires_at")?,
        fence_token: row.try_get_by("fence_token")?,
        checkpoint_id: row.try_get_by("checkpoint_id")?,
    }))
}

fn lease_deadline(now_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(LIVE_LEASE_MS)
        .context("coverage reservation lease timestamp overflow")
}

fn next_revision(revision: i64) -> Result<i64> {
    revision
        .checked_add(1)
        .context("coverage claim revision overflow")
}

/// Insert a brand-new `reserved_live` claim. Returns the reservation, or
/// `None` when a concurrent writer won the INSERT race (unique violation) —
/// the caller re-reads and re-decides.
#[allow(clippy::too_many_arguments)]
async fn try_insert_fresh_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    turn: &NormalizedTurn,
    coverage_schema_version: CoverageSchemaVersion,
    digest: &str,
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<Option<ReservedTurnClaim>> {
    let lease_expires_at = lease_deadline(now_ms)?;
    let reservation_state = reservation_state(source_channel)?;
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version,
                coverage_digest, completeness, revision, state,
                owner, lease_expires_at, fence_token, source_channel,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, 1, ?, ?, ?)
             ON CONFLICT(session_id, logical_turn_key, coverage_schema_version)
             DO NOTHING",
            [
                session_id.into(),
                turn.logical_turn_key.clone().into(),
                coverage_schema_version.as_i64().into(),
                digest.into(),
                turn.completeness.as_db_str().into(),
                reservation_state.into(),
                owner.into(),
                lease_expires_at.into(),
                source_channel.into(),
                now_ms.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("insert agent_coverage_claim reservation")?;
    if result.rows_affected() == 1 {
        Ok(Some(ReservedTurnClaim {
            logical_turn_key: turn.logical_turn_key.clone(),
            coverage_digest: digest.to_string(),
            completeness: turn.completeness,
            fence_token: 1,
            next_revision: 1,
        }))
    } else {
        Ok(None)
    }
}

/// Conditionally re-own an existing claim row (upgrade / takeover /
/// re-reserve). All prior identifying fields are in the WHERE so a concurrent
/// mutation makes this a 0-row no-op the caller re-reads after.
#[allow(clippy::too_many_arguments)]
async fn try_reown_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: CoverageSchemaVersion,
    expected_state: &str,
    expected_fence: Option<i64>,
    new_digest: &str,
    new_completeness: Completeness,
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<Option<(i64, i64)>> {
    let new_fence = expected_fence
        .unwrap_or(0)
        .checked_add(1)
        .context("coverage claim fence token overflow")?;
    let lease_expires_at = lease_deadline(now_ms)?;
    let expected_fence_value: sea_orm::Value = expected_fence.into();
    let reservation_state = reservation_state(source_channel)?;
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim
             SET state = ?, owner = ?, lease_expires_at = ?,
                 fence_token = ?, coverage_digest = ?, completeness = ?,
                 source_channel = ?, updated_at = ?
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ?
               AND state = ? AND fence_token IS ?",
            [
                reservation_state.into(),
                owner.into(),
                lease_expires_at.into(),
                new_fence.into(),
                new_digest.into(),
                new_completeness.as_db_str().into(),
                source_channel.into(),
                now_ms.into(),
                session_id.into(),
                logical_turn_key.into(),
                coverage_schema_version.as_i64().into(),
                expected_state.into(),
                expected_fence_value,
            ],
        ))
        .await
        .context("re-own agent_coverage_claim")?;
    if result.rows_affected() == 1 {
        Ok(Some((new_fence, lease_expires_at)))
    } else {
        Ok(None)
    }
}

fn reservation_state(source_channel: &str) -> Result<&'static str> {
    match source_channel {
        "import" => Ok("reserved_import"),
        "live" | "export" => Ok("reserved_live"),
        other => bail!("unsupported coverage source channel '{other}'"),
    }
}

/// Mark a committed-complete-vs-different-complete collision `conflicted`
/// (ADR-DR-09: never silently overwrite committed complete content).
#[allow(clippy::too_many_arguments)]
async fn try_mark_conflicted(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: CoverageSchemaVersion,
    incumbent: &ExistingClaim,
    incoming_turn: &NormalizedTurn,
    incoming_source_channel: &'static str,
    now_ms: i64,
    reasoning_changed: bool,
) -> Result<ConflictMarkOutcome> {
    let mut sanitized_turn = incoming_turn.clone();
    let redaction_report = redact_turns_with_report(std::slice::from_mut(&mut sanitized_turn));
    let encoded =
        encode_coverage_turn_for_schema(coverage_schema_version.as_i64(), &sanitized_turn)?;
    let incoming_digest = encoded.digest_hex;
    if incoming_digest == incumbent.coverage_digest && !reasoning_changed {
        return Ok(ConflictMarkOutcome::EquivalentAfterRedaction);
    }
    let incoming_canonical_json = String::from_utf8(encoded.canonical_bytes)
        .context("coverage conflict canonical evidence is not UTF-8")?;
    let incoming_redaction_report_json = if reasoning_changed {
        let mut report = serde_json::to_value(&redaction_report)
            .context("serialize coverage conflict redaction report")?;
        let object = report
            .as_object_mut()
            .context("coverage conflict redaction report is not an object")?;
        object.insert(
            "reason".to_owned(),
            REASONING_IDENTITY_CONFLICT_REASON.into(),
        );
        serde_json::to_string(&report).context("serialize coverage conflict redaction report")?
    } else {
        serde_json::to_string(&redaction_report)
            .context("serialize coverage conflict redaction report")?
    };
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim
         SET state = 'conflicted', updated_at = ?
         WHERE session_id = ? AND logical_turn_key = ?
           AND coverage_schema_version = ?
           AND state = 'catalog_committed' AND coverage_digest = ?
           AND revision = ? AND fence_token IS ? AND checkpoint_id IS ?",
            [
                now_ms.into(),
                session_id.into(),
                logical_turn_key.into(),
                coverage_schema_version.as_i64().into(),
                incumbent.coverage_digest.clone().into(),
                incumbent.revision.into(),
                incumbent.fence_token.into(),
                incumbent.checkpoint_id.clone().into(),
            ],
        ))
        .await
        .context("mark agent_coverage_claim conflicted")?;
    if result.rows_affected() != 1 {
        return Ok(ConflictMarkOutcome::LostRace);
    }
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_coverage_conflict (
            session_id, logical_turn_key, coverage_schema_version,
            incumbent_revision, incumbent_digest, incumbent_checkpoint_id,
            incoming_digest, incoming_source_channel, incoming_observed_at,
            incoming_canonical_json, incoming_redaction_report_json
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id, logical_turn_key, coverage_schema_version) DO NOTHING",
        [
            session_id.into(),
            logical_turn_key.into(),
            coverage_schema_version.as_i64().into(),
            incumbent.revision.into(),
            incumbent.coverage_digest.clone().into(),
            incumbent.checkpoint_id.clone().into(),
            incoming_digest.into(),
            incoming_source_channel.into(),
            now_ms.into(),
            incoming_canonical_json.into(),
            incoming_redaction_report_json.into(),
        ],
    ))
    .await
    .context("persist sanitized coverage conflict challenger")?;
    Ok(ConflictMarkOutcome::Marked)
}

enum ConflictMarkOutcome {
    Marked,
    EquivalentAfterRedaction,
    LostRace,
}

/// Reserve the turns of one live snapshot (ADR-DR-09 arbitration). Bounded:
/// each turn takes at most two decision rounds (initial read + one re-read
/// after losing a conditional write race).
pub async fn reserve_live_turn_claims(
    conn: &DatabaseConnection,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
) -> Result<LiveReservationOutcome> {
    let scope = CaptureScope::main_for_connection(conn).await?;
    reserve_live_turn_claims_with_capture_scope(conn, &scope, session_id, turns, owner, now_ms)
        .await
}

/// Scope-aware live reservation. Production callers must carry the scope
/// resolved at ingress so a released/fenced workspace cannot claim coverage
/// after it has lost ownership.
pub async fn reserve_live_turn_claims_with_capture_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
) -> Result<LiveReservationOutcome> {
    reserve_turn_claims_for_channel_with_capture_scope(
        conn, scope, session_id, turns, owner, now_ms, "live",
    )
    .await
}

/// Deadline-aware live reservation for managed hooks. Unlike historical
/// import, live capture must retain its one-transaction, all-or-nothing
/// claim plan; the deadline is therefore propagated through that single
/// transaction rather than splitting it into batches.
#[cfg(test)]
pub(crate) async fn reserve_live_turn_claims_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<LiveReservationOutcome> {
    reserve_turn_claims_for_channel_with_capture_scope_until(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        CaptureReservationExecution::live(deadline),
    )
    .await
}

/// The fixed capture channel and paired deadline that authorize one
/// deadline-aware reservation. Keeping these facts together prevents an
/// export path from accidentally applying the hook channel or a fresh budget.
#[derive(Clone, Copy)]
pub(crate) struct CaptureReservationExecution {
    source_channel: &'static str,
    deadline: CaptureCommitDeadline,
}

impl CaptureReservationExecution {
    pub(crate) const fn live(deadline: CaptureCommitDeadline) -> Self {
        Self {
            source_channel: "live",
            deadline,
        }
    }

    pub(crate) const fn export(deadline: CaptureCommitDeadline) -> Self {
        Self {
            source_channel: "export",
            deadline,
        }
    }
}

/// Deadline-aware scoped reservation with explicit provenance. This is the
/// managed-export counterpart to [`reserve_live_turn_claims_until`]: it
/// preserves the `export` channel while passing the established capture pair
/// through writer acquisition, pure reads, final authorization, and the
/// non-cancellable commit boundary.
#[cfg(test)]
pub(crate) async fn reserve_turn_claims_for_channel_with_capture_scope_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    execution: CaptureReservationExecution,
) -> Result<LiveReservationOutcome> {
    ensure_reservation_deadline(Some(execution.deadline), execution.source_channel)?;
    let outcome = reserve_turn_claims_for_channel_inner(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        execution.source_channel,
        Some(execution.deadline),
    )
    .await;
    match outcome {
        Err(error) if is_sqlite_busy(&error) => Ok(LiveReservationOutcome {
            skipped_inflight: turns.len(),
            ..LiveReservationOutcome::default()
        }),
        outcome => outcome,
    }
}

/// [`reserve_live_turn_claims`] with an explicit provenance channel
/// (`live` for hook events, `export` for the OpenCode bridge, `import` for
/// M4). The channel NEVER participates in arbitration (ADR-DR-09) — it is
/// recorded provenance only.
pub async fn reserve_turn_claims_for_channel(
    conn: &DatabaseConnection,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<LiveReservationOutcome> {
    let scope = CaptureScope::main_for_connection(conn).await?;
    reserve_turn_claims_for_channel_with_capture_scope(
        conn,
        &scope,
        session_id,
        turns,
        owner,
        now_ms,
        source_channel,
    )
    .await
}

/// Scope-aware reservation with explicit provenance. The lease is checked in
/// the same SQLite write transaction that inserts/re-owns the claim, rather
/// than only at the caller's preflight.
pub async fn reserve_turn_claims_for_channel_with_capture_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<LiveReservationOutcome> {
    let outcome = reserve_turn_claims_for_channel_inner(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        source_channel,
        None,
    )
    .await;
    match outcome {
        Err(error) if is_sqlite_busy(&error) => Ok(LiveReservationOutcome {
            skipped_inflight: turns.len(),
            ..LiveReservationOutcome::default()
        }),
        outcome => outcome,
    }
}

fn is_sqlite_busy(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("database is locked")
        || message.contains("database schema is locked")
        || message.contains("database table is locked")
        || message.contains("database is busy")
        || message.contains("sqlite_busy")
}

fn ensure_reservation_deadline(
    deadline: Option<CaptureCommitDeadline>,
    source_channel: &'static str,
) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        bail!("{source_channel} coverage reservation exceeded its execution deadline");
    }
    Ok(())
}

/// Bound only writer acquisition for a deadline-aware reservation. Claim DML
/// and its COMMIT remain outside this timeout because a cancelled dispatched
/// mutation cannot prove that SQLite did not durably accept it.
async fn begin_reservation_write_transaction_until(
    conn: &DatabaseConnection,
    deadline: Option<CaptureCommitDeadline>,
    source_channel: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_reservation_deadline(deadline, source_channel)?;
    let transaction = match deadline {
        Some(deadline) => tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic()),
            crate::internal::db::begin_write_transaction(conn),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!("{source_channel} coverage reservation exceeded its execution deadline")
        })?,
        None => crate::internal::db::begin_write_transaction(conn).await,
    };
    transaction.context("begin coverage reservation transaction")
}

/// Bound pre-DML reads while retaining the explicit rollback path owned by
/// the reservation transaction. This helper must never wrap claim mutation,
/// final authorization, or COMMIT.
async fn await_reservation_precommit_read_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    source_channel: &'static str,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_reservation_deadline(deadline, source_channel)?;
    match deadline {
        Some(deadline) => {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.monotonic()), read)
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "{source_channel} coverage reservation exceeded its execution deadline"
                    )
                })?
        }
        None => read.await,
    }
}

/// Derive the narrow paired deadline allowed for releasing a pre-existing
/// owned coverage reservation after capture work stops. If the foreground
/// deadline is still live, it remains the ceiling in both clock domains; a
/// fresh recovery deadline must never re-anchor its SQLite authorization.
fn coverage_abandonment_recovery_deadline(
    deadline: CaptureCommitDeadline,
) -> Result<CaptureCommitDeadline> {
    if deadline.monotonic() <= Instant::now() {
        return CaptureCommitDeadline::from_budget(COVERAGE_ABANDONMENT_RECOVERY_GRACE)
            .context("establish coverage reservation-abandonment recovery deadline");
    }

    let recovery_deadline = CaptureCommitDeadline::from_budget(COVERAGE_ABANDONMENT_RECOVERY_GRACE)
        .context("establish coverage reservation-abandonment recovery deadline")?;
    Ok(CaptureCommitDeadline::from_established_pair(
        deadline.monotonic().min(recovery_deadline.monotonic()),
        deadline
            .sqlite_not_after_millis()
            .min(recovery_deadline.sqlite_not_after_millis()),
    ))
}

/// Release every still-owned, uncommitted reservation after a writer aborts
/// before object construction. Fence tokens advance so a cancelled attempt
/// cannot later reuse its stale plan.
pub async fn abandon_reserved_turn_claims(
    conn: &DatabaseConnection,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
) -> Result<()> {
    let scope = CaptureScope::main_for_connection(conn).await?;
    abandon_reserved_turn_claims_with_capture_scope(
        conn,
        &scope,
        session_id,
        owner,
        source_channel,
        now_ms,
    )
    .await
}

/// Scope-aware abort cleanup. Releasing a reservation is itself a mutable
/// ownership transition, so it must not run after the workspace lease was
/// released or fenced.
pub async fn abandon_reserved_turn_claims_with_capture_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
) -> Result<()> {
    abandon_reserved_turn_claims_with_capture_scope_inner(
        conn,
        scope,
        session_id,
        owner,
        source_channel,
        now_ms,
        None,
    )
    .await
}

/// Deadline-aware managed-capture cleanup. The paired grace may release only
/// the caller's already-reserved rows; it bounds writer acquisition, pure
/// pre-DML reads, and the final SQLite authorization. Claim DML and COMMIT
/// remain non-cancellable so an expired acknowledgement cannot misreport a
/// durable abandonment as absent.
pub(crate) async fn abandon_reserved_turn_claims_with_capture_scope_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let recovery_deadline = coverage_abandonment_recovery_deadline(deadline)?;
    abandon_reserved_turn_claims_with_capture_scope_inner(
        conn,
        scope,
        session_id,
        owner,
        source_channel,
        now_ms,
        Some(recovery_deadline),
    )
    .await
}

/// Release every reservation `owner` still holds for `session_id` in the
/// caller's transaction; the single abandonment statement shared by managed
/// capture cleanup and the historical importer. Fence tokens advance so the
/// released owner cannot later commit its stale plan. The caller owns scope
/// verification, deadline checks, error context and the final commit.
pub(crate) async fn abandon_reserved_turn_claims_with_conn(
    conn: &impl ConnectionTrait,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
) -> Result<()> {
    let state = reservation_state(source_channel)?;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_coverage_claim
         SET state = 'abandoned', owner = NULL, lease_expires_at = NULL,
             attempt_checkpoint_id = NULL,
             fence_token = COALESCE(fence_token, 0) + 1, updated_at = ?
         WHERE session_id = ? AND state = ? AND owner = ?",
        [now_ms.into(), session_id.into(), state.into(), owner.into()],
    ))
    .await?;
    Ok(())
}

/// Extend every import reservation `owner` still holds for `session_id` in
/// the caller's attempt-binding transaction. A claim that a live writer
/// already preempted carries another owner and fence, so it is untouched.
pub(crate) async fn renew_import_turn_claim_leases_with_conn(
    conn: &impl ConnectionTrait,
    session_id: &str,
    owner: &str,
    lease_expires_at: i64,
    now_ms: i64,
) -> Result<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_coverage_claim SET lease_expires_at = ?, updated_at = ?
         WHERE session_id = ? AND state = 'reserved_import' AND owner = ?",
        [
            lease_expires_at.into(),
            now_ms.into(),
            session_id.into(),
            owner.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Record the checkpoint an import attempt is about to construct on its
/// fenced reservation, in the caller's attempt-binding transaction. Returns
/// `false` when the claim was abandoned or taken over since it was reserved.
pub(crate) async fn bind_import_turn_claim_attempt_with_conn(
    conn: &impl ConnectionTrait,
    session_id: &str,
    claim: &ReservedTurnClaim,
    owner: &str,
    checkpoint_id: &str,
    now_ms: i64,
) -> Result<bool> {
    bind_import_turn_claim_attempt_for_schema_with_conn(
        conn,
        session_id,
        CoverageSchemaVersion::V1,
        claim,
        owner,
        checkpoint_id,
        now_ms,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn bind_import_turn_claim_attempt_for_schema_with_conn(
    conn: &impl ConnectionTrait,
    session_id: &str,
    version: CoverageSchemaVersion,
    claim: &ReservedTurnClaim,
    owner: &str,
    checkpoint_id: &str,
    now_ms: i64,
) -> Result<bool> {
    let bound = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim SET attempt_checkpoint_id = ?, updated_at = ?
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ? AND state = 'reserved_import'
               AND owner = ? AND fence_token = ?",
            [
                checkpoint_id.into(),
                now_ms.into(),
                session_id.into(),
                claim.logical_turn_key.clone().into(),
                version.as_i64().into(),
                owner.into(),
                claim.fence_token.into(),
            ],
        ))
        .await?;
    Ok(bound.rows_affected() == 1)
}

#[allow(clippy::too_many_arguments)]
async fn abandon_reserved_turn_claims_with_capture_scope_inner(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<()> {
    // Reject an unknown channel before taking the SQLite writer lock.
    reservation_state(source_channel)?;
    let txn = begin_reservation_write_transaction_until(conn, deadline, source_channel)
        .await
        .context("begin coverage reservation abandonment transaction")?;
    if let Err(error) = await_reservation_precommit_read_until(
        deadline,
        source_channel,
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before abandoning coverage reservations");
    }
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    abandon_reserved_turn_claims_with_conn(&txn, session_id, owner, source_channel, now_ms)
        .await
        .context("abandon coverage reservations for aborted checkpoint writer")?;
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    match authorize_final_capture_commit(Some(scope), &txn, deadline).await {
        Ok(()) => {}
        Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed) => {
            txn.rollback().await.ok();
            bail!("{source_channel} coverage reservation exceeded its execution deadline");
        }
        Err(error) => {
            txn.rollback().await.ok();
            return Err(anyhow::Error::new(error)).context(
                "verify capture workspace lease before committing coverage reservation abandonment",
            );
        }
    }
    // Final authorization is the last SQL operation. Do not apply deadline
    // cancellation to COMMIT: SQLite may have dispatched it before a dropped
    // acknowledgement future, so the durable cleanup result would be unknown.
    txn.commit()
        .await
        .context("commit coverage reservation abandonment transaction")?;
    Ok(())
}

#[cfg(test)]
pub(crate) async fn reserve_import_turn_claims_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<LiveReservationOutcome> {
    reserve_import_turn_claims_with_reasoning_until(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        deadline,
        ReasoningCoverageInput::default(),
    )
    .await?
    .into_ordinary()
}

/// Keep the existing bounded import batches, while a reasoning-partial
/// batch prevents every owned batch from reaching a checkpoint writer.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reserve_import_turn_claims_with_reasoning_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
    input: ReasoningCoverageInput<'_>,
) -> Result<ReasoningReservationOutcome> {
    reserve_import_turn_claims_with_reasoning_versions_until(
        conn,
        scope,
        session_id,
        turns,
        None,
        owner,
        now_ms,
        deadline,
        input,
        CoverageVersionAdmission::Automatic,
    )
    .await
}

/// Only the explicit prepared-import adapter may override automatic ordinary
/// version selection. Existing live/export/default-import entry points do not.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reserve_import_turn_claims_by_version_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    versions: &[i64],
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
    input: ReasoningCoverageInput<'_>,
) -> Result<ReasoningReservationOutcome> {
    ensure!(
        versions.len() == turns.len(),
        "import coverage version count differs from turn count"
    );
    reserve_import_turn_claims_with_reasoning_versions_until(
        conn,
        scope,
        session_id,
        turns,
        Some(versions),
        owner,
        now_ms,
        deadline,
        input,
        CoverageVersionAdmission::ExplicitOrdinaryImport,
    )
    .await
}

/// Private production import route: rejected history is checked under the
/// first reservation writer lock. Empty snapshots use a read-only preflight.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reserve_runtime_import_turn_claims_with_reasoning_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
    input: ReasoningCoverageInput<'_>,
    source_rejected: bool,
) -> Result<ReasoningReservationOutcome> {
    ensure_reservation_deadline(Some(deadline), "import")?;
    if turns
        .iter()
        .any(|turn| select_coverage_schema(turn) == CoverageSchemaVersion::V2)
    {
        ensure!(
            turns.len() <= crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES,
            "runtime versioned coverage claim count exceeds its safe limit; start a new capture session"
        );
    }
    if let Some(reader) = &input.reader {
        reader.validate_for(scope, session_id)?;
    }
    if turns.is_empty() {
        ensure_runtime_source_admitted_until(
            conn,
            scope,
            session_id,
            deadline,
            "import",
            source_rejected,
        )
        .await?;
    }
    reserve_import_turn_claims_with_reasoning_versions_until(
        conn,
        scope,
        session_id,
        turns,
        None,
        owner,
        now_ms,
        deadline,
        input,
        CoverageVersionAdmission::Runtime { source_rejected },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn reserve_import_turn_claims_with_reasoning_versions_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    versions: Option<&[i64]>,
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
    input: ReasoningCoverageInput<'_>,
    admission: CoverageVersionAdmission,
) -> Result<ReasoningReservationOutcome> {
    if let Some(candidate) = input.candidate {
        candidate.validate_for(scope, session_id, turns)?;
    }
    if let Some(candidate) = input.versioned_candidate {
        candidate.validate_for(scope, session_id, turns)?;
    }
    if let Some(reader) = &input.reader {
        reader.validate_for(scope, session_id)?;
    }
    // Bound SQLite writer-lock residency and re-establish the tombstone
    // barrier between batches. Earlier batches remain explicitly owned and
    // are abandoned by the importer if a later batch reaches the deadline.
    const IMPORT_RESERVATION_BATCH_TURNS: usize = 64;
    let mut aggregate = ReasoningReservationOutcome::default();
    for (batch_index, batch) in turns.chunks(IMPORT_RESERVATION_BATCH_TURNS).enumerate() {
        ensure_reservation_deadline(Some(deadline), "import")?;
        // Recheck rejected history under the first reservation writer lock.
        // The attempt was admitted before its first claim; later bounded
        // batches preserve per-key no-demotion, scope and tombstone fences.
        let batch_admission = match admission {
            CoverageVersionAdmission::Runtime { .. } if batch_index != 0 => {
                CoverageVersionAdmission::Runtime {
                    source_rejected: false,
                }
            }
            admission => admission,
        };
        let outcome = match versions {
            Some(versions) => {
                let offset = batch_index * IMPORT_RESERVATION_BATCH_TURNS;
                reserve_turn_claims_with_reasoning_inner_versioned(
                    conn,
                    scope,
                    session_id,
                    batch,
                    Some(&versions[offset..offset + batch.len()]),
                    owner,
                    now_ms,
                    "import",
                    Some(deadline),
                    input.clone(),
                    batch_admission,
                )
                .await?
            }
            None => {
                reserve_turn_claims_with_reasoning_inner_versioned(
                    conn,
                    scope,
                    session_id,
                    batch,
                    None,
                    owner,
                    now_ms,
                    "import",
                    Some(deadline),
                    input.clone(),
                    batch_admission,
                )
                .await?
            }
        };
        #[cfg(test)]
        if let Some(delay) = import_reservation_batch_test_delay() {
            tokio::time::sleep(delay).await;
        }
        aggregate.skipped_incomplete_reasoning = aggregate
            .skipped_incomplete_reasoning
            .saturating_add(outcome.skipped_incomplete_reasoning);
        aggregate
            .ordinary
            .reserved
            .extend(outcome.ordinary.reserved);
        aggregate
            .versioned_reserved
            .extend(outcome.versioned_reserved);
        aggregate.covered.extend(outcome.covered);
        aggregate.ordinary.skipped_covered = aggregate
            .ordinary
            .skipped_covered
            .saturating_add(outcome.ordinary.skipped_covered);
        aggregate.ordinary.skipped_inflight = aggregate
            .ordinary
            .skipped_inflight
            .saturating_add(outcome.ordinary.skipped_inflight);
        aggregate.ordinary.skipped_retained = aggregate
            .ordinary
            .skipped_retained
            .saturating_add(outcome.ordinary.skipped_retained);
        aggregate.ordinary.conflicted = aggregate
            .ordinary
            .conflicted
            .saturating_add(outcome.ordinary.conflicted);
        if aggregate.skipped_incomplete_reasoning != 0 {
            if !aggregate.ordinary.reserved.is_empty() {
                abandon_reserved_turn_claims_with_capture_scope_until(
                    conn, scope, session_id, owner, "import", now_ms, deadline,
                )
                .await
                .context("release earlier import batches after partial reasoning coverage")?;
                aggregate.ordinary.reserved.clear();
                aggregate.versioned_reserved.clear();
            }
            return Ok(aggregate);
        }
    }
    Ok(aggregate)
}

#[allow(clippy::too_many_arguments)]
async fn reserve_turn_claims_for_channel_inner(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<LiveReservationOutcome> {
    reserve_turn_claims_with_reasoning_inner(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        source_channel,
        deadline,
        ReasoningCoverageInput::default(),
    )
    .await?
    .into_ordinary()
}

/// Shared live/export/import reservation, retaining the typed partial state.
/// The legacy wrappers use the same transaction and reject any partial rather
/// than silently erasing the new outcome.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reserve_turn_claims_with_reasoning(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    execution: CaptureReservationExecution,
    input: ReasoningCoverageInput<'_>,
) -> Result<ReasoningReservationOutcome> {
    ensure_reservation_deadline(Some(execution.deadline), execution.source_channel)?;
    reserve_turn_claims_with_reasoning_inner(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        execution.source_channel,
        Some(execution.deadline),
        input,
    )
    .await
}

/// Runtime selection happens only after the authorized source has produced
/// redacted turns. The rejection flag is content-free and cannot authorize v2.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reserve_runtime_turn_claims_with_reasoning(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    execution: CaptureReservationExecution,
    input: ReasoningCoverageInput<'_>,
    source_rejected: bool,
) -> Result<ReasoningReservationOutcome> {
    ensure_reservation_deadline(Some(execution.deadline), execution.source_channel)?;
    reserve_turn_claims_with_reasoning_inner_versioned(
        conn,
        scope,
        session_id,
        turns,
        None,
        owner,
        now_ms,
        execution.source_channel,
        Some(execution.deadline),
        input,
        CoverageVersionAdmission::Runtime { source_rejected },
    )
    .await
}

/// Content-free admission for authorized partial/empty snapshots that will
/// not enter a reservation gate. Source/native authorization remains the
/// caller's prerequisite; this function does not manufacture empty identity.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ensure_runtime_source_admitted_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    deadline: CaptureCommitDeadline,
    source_channel: &'static str,
    source_rejected: bool,
) -> Result<()> {
    reservation_state(source_channel)?;
    await_reservation_precommit_read_until(Some(deadline), source_channel, async {
        scope.assert_workspace_fence_live(conn).await?;
        let writable = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_session s WHERE s.session_id = ? AND NOT EXISTS (
             SELECT 1 FROM agent_import_tombstone t WHERE t.agent_kind = s.agent_kind
             AND t.provider_session_id = s.provider_session_id)",
                [session_id.into()],
            ))
            .await
            .context("verify runtime coverage tombstone barrier")?;
        ensure!(
            writable.is_some(),
            "agent session was erased or is unavailable for coverage reservation"
        );
        if source_rejected {
            ensure_no_rejected_runtime_v2_history(conn, session_id).await?;
        }
        Ok(())
    })
    .await
}

/// Import needs this history check before identity migration creates its
/// session row. Source/native/storage authorization remains the caller's
/// prerequisite; the later reservation rechecks tombstones and history under
/// its writer lock. Missing session rows cannot hide existing claim history.
pub(crate) async fn ensure_runtime_import_source_history_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    deadline: CaptureCommitDeadline,
    source_rejected: bool,
) -> Result<()> {
    await_reservation_precommit_read_until(Some(deadline), "import", async {
        scope
            .assert_workspace_fence_live(conn)
            .await
            .context("verify capture workspace fence before import history preflight")?;
        if source_rejected {
            ensure_no_rejected_runtime_v2_history(conn, session_id).await?;
        }
        Ok(())
    })
    .await
}

/// Prefix-index range scan: the version is the third component of the
/// existing session/key/version index. LIMIT bounds returned data, while the
/// enclosing deadline bounds work across this session's historical rows.
async fn ensure_no_rejected_runtime_v2_history(
    conn: &impl ConnectionTrait,
    session_id: &str,
) -> Result<()> {
    let present = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM agent_coverage_claim INDEXED BY idx_agent_coverage_claim_logical_key
         WHERE session_id = ? AND coverage_schema_version = 2 LIMIT 1",
            [session_id.into()],
        ))
        .await
        .context("cannot check rejected capture coverage history; run libra agent doctor")?;
    ensure!(
        present.is_none(),
        "capture source classification was rejected after coverage-v2 history; start a new session or run libra agent doctor"
    );
    Ok(())
}

/// Shared scoped reservation for in-process callers that have no enclosing
/// managed capture deadline. The typed partial outcome is preserved just as
/// it is on the deadline-aware path.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reserve_turn_claims_with_reasoning_no_deadline(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
    input: ReasoningCoverageInput<'_>,
) -> Result<ReasoningReservationOutcome> {
    reserve_turn_claims_with_reasoning_inner(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        source_channel,
        None,
        input,
    )
    .await
}

/// Reserve explicit per-turn coverage versions without activating a provider
/// Session. `versions[i]` names `turns[i]`; invalid versions and v1 typed
/// Reasoning reject before the first database write. This entry point is the
/// versioned arbitration seam later capture/import cards will consume.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn reserve_turn_claims_by_version_with_capture_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    versions: &[i64],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<VersionedReservationOutcome> {
    let outcome = reserve_turn_claims_with_reasoning_inner_versioned(
        conn,
        scope,
        session_id,
        turns,
        Some(versions),
        owner,
        now_ms,
        source_channel,
        None,
        ReasoningCoverageInput::default(),
        CoverageVersionAdmission::Automatic,
    )
    .await;
    match outcome {
        Err(error) if is_sqlite_busy(&error) => Ok(VersionedReservationOutcome {
            skipped_inflight: turns.len(),
            ..VersionedReservationOutcome::default()
        }),
        outcome => outcome?.into_versioned(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn reserve_turn_claims_with_reasoning_inner(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
    deadline: Option<CaptureCommitDeadline>,
    reasoning: ReasoningCoverageInput<'_>,
) -> Result<ReasoningReservationOutcome> {
    reserve_turn_claims_with_reasoning_inner_versioned(
        conn,
        scope,
        session_id,
        turns,
        None,
        owner,
        now_ms,
        source_channel,
        deadline,
        reasoning,
        CoverageVersionAdmission::Automatic,
    )
    .await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CoverageVersionAdmission {
    Automatic,
    ExplicitOrdinaryImport,
    Runtime { source_rejected: bool },
}

#[allow(clippy::too_many_arguments)]
async fn reserve_turn_claims_with_reasoning_inner_versioned(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    versions: Option<&[i64]>,
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
    deadline: Option<CaptureCommitDeadline>,
    reasoning: ReasoningCoverageInput<'_>,
    admission: CoverageVersionAdmission,
) -> Result<ReasoningReservationOutcome> {
    ensure!(
        versions.is_none_or(|versions| versions.len() == turns.len()),
        "coverage claim version count differs from turn count"
    );
    if matches!(admission, CoverageVersionAdmission::Runtime { .. })
        && turns
            .iter()
            .any(|turn| select_coverage_schema(turn) == CoverageSchemaVersion::V2)
    {
        ensure!(
            turns.len() <= crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES,
            "runtime versioned coverage claim count exceeds its safe limit; start a new capture session"
        );
    }
    let mut checked_versions = Vec::with_capacity(turns.len());
    let mut seen_versioned_keys = std::collections::BTreeSet::new();
    for (index, turn) in turns.iter().enumerate() {
        let version = CoverageSchemaVersion::try_from(versions.map_or_else(
            || match admission {
                CoverageVersionAdmission::Runtime { .. } => select_coverage_schema(turn).as_i64(),
                _ => COVERAGE_SCHEMA_VERSION,
            },
            |versions| versions[index],
        ))?;
        ensure!(
            version == select_coverage_schema(turn)
                || (admission == CoverageVersionAdmission::ExplicitOrdinaryImport
                    && source_channel == "import"
                    && versions.is_some()
                    && version == CoverageSchemaVersion::V2
                    && select_coverage_schema(turn) == CoverageSchemaVersion::V1),
            "coverage claim version does not match the typed turn"
        );
        if versions.is_some() || matches!(admission, CoverageVersionAdmission::Runtime { .. }) {
            ensure!(
                seen_versioned_keys.insert((version.as_i64(), turn.logical_turn_key.as_str())),
                "duplicate versioned coverage claim in one reservation"
            );
        }
        checked_versions.push(version);
    }
    if let Some(candidate) = reasoning.candidate {
        candidate.validate_for(scope, session_id, turns)?;
        ensure!(
            checked_versions
                .iter()
                .all(|version| *version == CoverageSchemaVersion::V1),
            "reasoning coverage source version does not match its claims; run libra agent doctor"
        );
    }
    if let Some(candidate) = reasoning.versioned_candidate {
        candidate.validate_for(scope, session_id, turns)?;
        ensure!(
            turns.iter().zip(&checked_versions).all(|(turn, version)| {
                candidate.turn(*version, &turn.logical_turn_key).is_some()
            }),
            "reasoning coverage source version does not match its claims; run libra agent doctor"
        );
    }
    if matches!(admission, CoverageVersionAdmission::Runtime { .. }) {
        ensure!(
            reasoning.reader.is_some(),
            "runtime capture requires a scoped reasoning identity reader; run libra agent doctor"
        );
    }
    if let Some(reader) = &reasoning.reader {
        reader.validate_for(scope, session_id)?;
    }
    // ADR-DR-19: reserve the complete snapshot under one SQLite writer
    // transaction and establish the tombstone barrier before any claim
    // mutation. This closes both fresh-INSERT and re-own races with erasure.
    // Reads the tombstone barrier before it writes the claim, so the write
    // lock is taken up front (`db::begin_write_transaction`).
    let txn = begin_reservation_write_transaction_until(conn, deadline, source_channel).await?;
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    if let Err(error) = await_reservation_precommit_read_until(
        deadline,
        source_channel,
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before reserving coverage claims");
    }
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    let writable = match await_reservation_precommit_read_until(deadline, source_channel, async {
        txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 AS writable
                 FROM agent_session s
                 WHERE s.session_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_import_tombstone t
                     WHERE t.agent_kind = s.agent_kind
                       AND t.provider_session_id = s.provider_session_id
                   )",
            [session_id.into()],
        ))
        .await
        .context("verify coverage reservation tombstone barrier")
    })
    .await
    {
        Ok(writable) => writable,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    if writable.is_none() {
        txn.rollback().await.ok();
        bail!("agent session was erased or is unavailable for coverage reservation");
    }
    if let CoverageVersionAdmission::Runtime {
        source_rejected: true,
    } = admission
        && let Err(error) = await_reservation_precommit_read_until(
            deadline,
            source_channel,
            ensure_no_rejected_runtime_v2_history(&txn, session_id),
        )
        .await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    let map = match await_reservation_precommit_read_until(
        deadline,
        source_channel,
        read_reasoning_map(&txn, scope, session_id, reasoning.reader.as_ref()),
    )
    .await
    {
        Ok(map) => map,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if matches!(map, StoredReasoningMap::Invalid) {
        txn.rollback().await.ok();
        bail!("private reasoning identity is invalid; start a new capture session");
    }
    // Native identity is not portable between schema domains. A fresh target
    // claim must not evade the incumbent native proof in the other domain.
    if matches!(
        admission,
        CoverageVersionAdmission::ExplicitOrdinaryImport | CoverageVersionAdmission::Runtime { .. }
    ) && let StoredReasoningMap::Verified(maps) = &map
    {
        let reader = reasoning
            .reader
            .as_ref()
            .context("explicit import requires a scoped reasoning reader")?;
        for (turn, version) in turns.iter().zip(&checked_versions) {
            let native_alias = match version {
                CoverageSchemaVersion::V1 => {
                    if let Some(other) = &maps.v2 {
                        let slot = await_reservation_precommit_read_until(
                            deadline,
                            source_channel,
                            reader.turn_slot_for_schema_version(&txn, 2, &turn.logical_turn_key),
                        )
                        .await?;
                        other.records().get(&slot).is_some_and(|record| {
                            record.body().kind == ReasoningIdentityKind::Nonempty
                        })
                    } else {
                        false
                    }
                }
                CoverageSchemaVersion::V2 => {
                    if let Some(other) = &maps.v1 {
                        let slot = await_reservation_precommit_read_until(
                            deadline,
                            source_channel,
                            reader.turn_slot(&txn, &turn.logical_turn_key),
                        )
                        .await?;
                        other.records().get(&slot).is_some_and(|record| {
                            record.body().kind == ReasoningIdentityKind::Nonempty
                        })
                    } else {
                        false
                    }
                }
            };
            if native_alias {
                txn.rollback().await.ok();
                bail!(
                    "native reasoning cannot be imported across coverage versions; use the original version or run libra agent doctor"
                );
            }
        }
    }
    let mut outcome = ReasoningReservationOutcome::default();
    for (turn, version) in turns.iter().zip(checked_versions) {
        if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
            txn.rollback().await.ok();
            return Err(error);
        }
        if version == CoverageSchemaVersion::V1
            && matches!(admission, CoverageVersionAdmission::Runtime { .. })
            && await_reservation_precommit_read_until(
                deadline,
                source_channel,
                read_claim(
                    &txn,
                    session_id,
                    &turn.logical_turn_key,
                    CoverageSchemaVersion::V2,
                ),
            )
            .await?
            .is_some()
        {
            txn.rollback().await.ok();
            bail!(
                "coverage-v2 history cannot be downgraded by capture; start a new session or run libra agent doctor"
            );
        }
        let digest = encode_coverage_turn_for_schema(version.as_i64(), turn)?.digest_hex;
        let mut rounds = 0;
        loop {
            if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
                txn.rollback().await.ok();
                return Err(error);
            }
            rounds += 1;
            let existing = match await_reservation_precommit_read_until(
                deadline,
                source_channel,
                read_claim(&txn, session_id, &turn.logical_turn_key, version),
            )
            .await
            {
                Ok(existing) => existing,
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error);
                }
            };
            if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
                txn.rollback().await.ok();
                return Err(error);
            }
            let decision = match decide_reasoning_and_attempt(
                &txn,
                session_id,
                turn,
                version,
                &digest,
                owner,
                now_ms,
                existing,
                source_channel,
                scope,
                &map,
                &reasoning,
            )
            .await
            {
                Ok(decision) => decision,
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error);
                }
            };
            if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
                txn.rollback().await.ok();
                return Err(error);
            }
            match decision {
                AttemptOutcome::Reserved(claim) => {
                    outcome.versioned_reserved.push(VersionedReservedTurnClaim {
                        coverage_schema_version: version,
                        claim: claim.clone(),
                    });
                    outcome.ordinary.reserved.push(claim);
                    break;
                }
                AttemptOutcome::SkipCovered(committed) => {
                    // Import retention can append ordinary source values
                    // without advancing a covered semantic claim. Carry the
                    // same gate-issued observation so both its marker bind
                    // and final transaction can revalidate the incumbent.
                    if matches!(admission, CoverageVersionAdmission::Runtime { .. })
                        || (source_channel == "import" && reasoning.ordinary_retention_observations)
                    {
                        outcome
                            .covered
                            .push(CommittedTurnClaimSnapshot::from_committed(
                                session_id,
                                &turn.logical_turn_key,
                                version,
                                committed,
                                reasoning.candidate.is_some()
                                    || reasoning.versioned_candidate.is_some(),
                            )?);
                    }
                    outcome.ordinary.skipped_covered += 1;
                    break;
                }
                AttemptOutcome::SkipInflight => {
                    outcome.ordinary.skipped_inflight += 1;
                    break;
                }
                AttemptOutcome::SkipRetained => {
                    outcome.ordinary.skipped_inflight += 1;
                    outcome.ordinary.skipped_retained += 1;
                    break;
                }
                AttemptOutcome::Conflicted => {
                    outcome.ordinary.conflicted += 1;
                    break;
                }
                AttemptOutcome::SkipIncompleteReasoning => {
                    outcome.skipped_incomplete_reasoning += 1;
                    break;
                }
                AttemptOutcome::LostRace if rounds < 3 => continue,
                AttemptOutcome::LostRace => {
                    // Two consecutive lost races: someone very active owns
                    // this turn right now; treat as in-flight, next event
                    // will retry.
                    outcome.ordinary.skipped_inflight += 1;
                    break;
                }
            }
        }
        #[cfg(test)]
        if source_channel == "live"
            && let Some(delay) = live_reservation_turn_test_delay()
        {
            tokio::time::sleep(delay).await;
        }
    }
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    if outcome.skipped_incomplete_reasoning != 0 {
        // A mixed snapshot is not permission to publish its reserved subset.
        // Roll back fresh reservations and conflict mutations together. The
        // caller receives only the durable, non-mutating skip observations.
        txn.rollback()
            .await
            .context("roll back partial reasoning coverage reservation")?;
        outcome.ordinary.reserved.clear();
        outcome.versioned_reserved.clear();
        outcome.ordinary.conflicted = 0;
        return Ok(outcome);
    }
    match authorize_final_capture_commit(Some(scope), &txn, deadline).await {
        Ok(()) => {}
        Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed) => {
            txn.rollback().await.ok();
            bail!("{source_channel} coverage reservation exceeded its execution deadline");
        }
        Err(error) => {
            txn.rollback().await.ok();
            return Err(anyhow::Error::new(error))
                .context("verify capture workspace lease before committing coverage reservation");
        }
    }
    // The authorization above is the last SQL statement. Do not recheck or
    // cancel around this commit: SQLite may already have dispatched COMMIT,
    // so the acknowledgement must be awaited to avoid reporting a timeout
    // after durable claims were published.
    txn.commit()
        .await
        .context("commit coverage reservation transaction")?;
    Ok(outcome)
}

enum AttemptOutcome {
    Reserved(ReservedTurnClaim),
    SkipCovered(ExistingClaim),
    SkipInflight,
    SkipRetained,
    Conflicted,
    LostRace,
    SkipIncompleteReasoning,
}

#[allow(clippy::too_many_arguments)]
async fn decide_reasoning_and_attempt(
    conn: &impl ConnectionTrait,
    session_id: &str,
    turn: &NormalizedTurn,
    coverage_schema_version: CoverageSchemaVersion,
    digest: &str,
    owner: &str,
    now_ms: i64,
    existing: Option<ExistingClaim>,
    source_channel: &'static str,
    scope: &CaptureScope,
    map: &StoredReasoningMap,
    input: &ReasoningCoverageInput<'_>,
) -> Result<AttemptOutcome> {
    if let Some(incumbent) = &existing {
        if incumbent.state == "catalog_committed" {
            let comparison = compare_reasoning_identity(
                conn,
                scope,
                session_id,
                &turn.logical_turn_key,
                coverage_schema_version,
                incumbent,
                input
                    .versioned_candidate
                    .and_then(|plan| plan.turn(coverage_schema_version, &turn.logical_turn_key))
                    .or_else(|| {
                        input.candidate.and_then(|plan| {
                            (coverage_schema_version == CoverageSchemaVersion::V1)
                                .then(|| plan.turn(&turn.logical_turn_key))
                                .flatten()
                        })
                    }),
                map,
                input.reader.as_ref(),
            )
            .await?;
            match comparison {
                ReasoningComparison::Changed | ReasoningComparison::Partial => {
                    let changed = matches!(comparison, ReasoningComparison::Changed);
                    if incumbent.completeness == "complete"
                        && turn.completeness == Completeness::Complete
                        && (changed || incumbent.coverage_digest != digest)
                    {
                        return Ok(
                            match try_mark_conflicted(
                                conn,
                                session_id,
                                &turn.logical_turn_key,
                                coverage_schema_version,
                                incumbent,
                                turn,
                                source_channel,
                                now_ms,
                                changed,
                            )
                            .await?
                            {
                                ConflictMarkOutcome::Marked => AttemptOutcome::Conflicted,
                                ConflictMarkOutcome::LostRace => AttemptOutcome::LostRace,
                                ConflictMarkOutcome::EquivalentAfterRedaction => {
                                    AttemptOutcome::SkipIncompleteReasoning
                                }
                            },
                        );
                    }
                    if !changed || turn.completeness == Completeness::Incomplete {
                        return Ok(AttemptOutcome::SkipIncompleteReasoning);
                    }
                    // Authenticated incomplete -> complete retains the
                    // incumbent revision/fence upgrade below, even if changed.
                }
                ReasoningComparison::Ordinary | ReasoningComparison::Same => {}
            }
        } else if matches!(map, StoredReasoningMap::Invalid) {
            return Ok(AttemptOutcome::SkipIncompleteReasoning);
        }
    } else if matches!(map, StoredReasoningMap::Invalid) {
        return Ok(AttemptOutcome::SkipIncompleteReasoning);
    }
    decide_and_attempt(
        conn,
        session_id,
        turn,
        coverage_schema_version,
        digest,
        owner,
        now_ms,
        existing,
        source_channel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn decide_and_attempt(
    conn: &impl ConnectionTrait,
    session_id: &str,
    turn: &NormalizedTurn,
    coverage_schema_version: CoverageSchemaVersion,
    digest: &str,
    owner: &str,
    now_ms: i64,
    existing: Option<ExistingClaim>,
    source_channel: &'static str,
) -> Result<AttemptOutcome> {
    let Some(existing) = existing else {
        return Ok(
            match try_insert_fresh_claim(
                conn,
                session_id,
                turn,
                coverage_schema_version,
                digest,
                owner,
                now_ms,
                source_channel,
            )
            .await?
            {
                Some(claim) => AttemptOutcome::Reserved(claim),
                None => AttemptOutcome::LostRace,
            },
        );
    };

    match existing.state.as_str() {
        "catalog_committed" => {
            let completeness_upgrade = existing.completeness == "incomplete"
                && turn.completeness == Completeness::Complete;
            if existing.coverage_digest == digest && !completeness_upgrade {
                // Equivalent content/completeness is a pure no-op. Terminal
                // evidence is deliberately outside the semantic digest, so a
                // same-digest incomplete -> complete transition must still
                // advance the revision and session lifecycle.
                return Ok(AttemptOutcome::SkipCovered(existing));
            }
            match (existing.completeness.as_str(), turn.completeness) {
                ("incomplete", Completeness::Complete) => {
                    // Upgrade path: incomplete → complete advances the turn's
                    // current revision (ADR-DR-16).
                    match try_reown_claim(
                        conn,
                        session_id,
                        &turn.logical_turn_key,
                        coverage_schema_version,
                        "catalog_committed",
                        existing.fence_token,
                        digest,
                        turn.completeness,
                        owner,
                        now_ms,
                        source_channel,
                    )
                    .await?
                    {
                        Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                            logical_turn_key: turn.logical_turn_key.clone(),
                            coverage_digest: digest.to_string(),
                            completeness: turn.completeness,
                            fence_token: fence,
                            next_revision: next_revision(existing.revision)?,
                        })),
                        None => Ok(AttemptOutcome::LostRace),
                    }
                }
                ("complete", Completeness::Complete) => {
                    // complete → different complete: never auto-overwrite.
                    let conflict = try_mark_conflicted(
                        conn,
                        session_id,
                        &turn.logical_turn_key,
                        coverage_schema_version,
                        &existing,
                        turn,
                        source_channel,
                        now_ms,
                        false,
                    )
                    .await?;
                    Ok(match conflict {
                        ConflictMarkOutcome::Marked => AttemptOutcome::Conflicted,
                        ConflictMarkOutcome::EquivalentAfterRedaction => {
                            AttemptOutcome::SkipCovered(existing)
                        }
                        ConflictMarkOutcome::LostRace => AttemptOutcome::LostRace,
                    })
                }
                // A (different) incomplete snapshot never downgrades or
                // replaces committed content.
                (_, Completeness::Incomplete) => Ok(AttemptOutcome::SkipCovered(existing)),
                // incomplete → incomplete with different digest: keep the
                // committed one; a later complete parse upgrades it.
                _ => Ok(AttemptOutcome::SkipCovered(existing)),
            }
        }
        "reserved_live" | "reserved_import" => {
            // A durable pending terminal artifact owns these turns until its
            // replay commits them or an explicit erase abandons them. Taking
            // them over would advance the fence the artifact must present,
            // leaving its authenticated snapshot permanently unreplayable.
            if existing.state == "reserved_live"
                && existing.lease_expires_at == Some(ARTIFACT_RETAINED_LEASE_MS)
            {
                return Ok(AttemptOutcome::SkipRetained);
            }
            let lease_live = existing.lease_expires_at.is_some_and(|t| t > now_ms);
            // A live hook/export writer may preempt an import reservation
            // before the import has committed the traces ref (ADR-DR-09).
            // The conditional re-own increments the fence so the stale import
            // holder cannot later update ref/claim/identity. Import never
            // preempts a live writer.
            let live_preempts_import =
                existing.state == "reserved_import" && matches!(source_channel, "live" | "export");
            if lease_live && !live_preempts_import {
                return Ok(AttemptOutcome::SkipInflight);
            }
            // Expired lease: fenced takeover (stale holder's later writes
            // fail their fence check and roll back).
            match try_reown_claim(
                conn,
                session_id,
                &turn.logical_turn_key,
                coverage_schema_version,
                &existing.state,
                existing.fence_token,
                digest,
                turn.completeness,
                owner,
                now_ms,
                source_channel,
            )
            .await?
            {
                Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_digest: digest.to_string(),
                    completeness: turn.completeness,
                    fence_token: fence,
                    next_revision: next_revision(existing.revision)?,
                })),
                None => Ok(AttemptOutcome::LostRace),
            }
        }
        "abandoned" => {
            match try_reown_claim(
                conn,
                session_id,
                &turn.logical_turn_key,
                coverage_schema_version,
                "abandoned",
                existing.fence_token,
                digest,
                turn.completeness,
                owner,
                now_ms,
                source_channel,
            )
            .await?
            {
                Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_digest: digest.to_string(),
                    completeness: turn.completeness,
                    fence_token: fence,
                    next_revision: next_revision(existing.revision)?,
                })),
                None => Ok(AttemptOutcome::LostRace),
            }
        }
        // Conflicted rows stay parked for doctor; never auto-resolved here.
        _ => Ok(AttemptOutcome::Conflicted),
    }
}

/// The transactional commit plan for one gated checkpoint write: applied by
/// `HistoryManager` INSIDE the ref-CAS transaction (ADR-DR-10 — ref update,
/// catalog row, coverage revisions and claim advances all commit or all roll
/// back together).
#[derive(Clone)]
pub struct LiveClaimCommitPlan {
    /// Provenance channel recorded on revisions ('live' | 'export' | 'import').
    pub source_channel: &'static str,
    pub session_id: String,
    pub checkpoint_id: String,
    pub owner: String,
    pub parent_commit: Option<String>,
    pub created_at: i64,
    pub now_ms: i64,
    pub claims: Vec<ReservedTurnClaim>,
    /// Import-only session lifecycle/ownership update, committed with the
    /// first visible checkpoint instead of during lease acquisition.
    pub import_session: Option<CaptureImportSessionCommit>,
    /// Import-only identity cursor/fence update. `None` for live/export.
    pub import_identity: Option<ImportIdentityCommit>,
    /// Scope resolved at capture ingress. A final ref-CAS transaction must
    /// re-check the workspace lease because object construction can outlive
    /// the earlier reservation transaction.
    pub capture_scope: Option<CaptureScope>,
}

/// Identity records are a transaction companion, never an eager metadata
/// update. The existing ref/catalog/claim companion remains authoritative;
/// failing either half rolls the enclosing checkpoint transaction back.
pub(crate) struct ReasoningClaimCommitPlan {
    ordinary: LiveClaimCommitPlan,
    reasoning: ReasoningCoveragePlan,
}

/// One live/export checkpoint's existing claim companion with an optional
/// sealed source-derived identity plan. Ordinary captures preserve the exact
/// legacy companion; native-proven captures apply both through the same
/// traces ref-CAS transaction.
pub(crate) struct LiveCaptureClaimCommitPlan {
    ordinary: LiveClaimCommitPlan,
    reasoning: Option<ReasoningCoveragePlan>,
    versioned: Option<VersionedLiveClaimCommitPlan>,
}

impl LiveCaptureClaimCommitPlan {
    pub(crate) fn new(
        ordinary: LiveClaimCommitPlan,
        reasoning: Option<&ReasoningCoveragePlan>,
    ) -> Result<Self> {
        if let Some(reasoning) = reasoning {
            let scope = ordinary
                .capture_scope
                .as_ref()
                .context("reasoning coverage requires the current capture scope")?;
            reasoning.validate_for(scope, &ordinary.session_id, &[])?;
            if ordinary
                .claims
                .iter()
                .any(|claim| reasoning.turn(&claim.logical_turn_key).is_none())
            {
                bail!(
                    "reasoning coverage source does not match its reserved turns; run libra agent doctor"
                );
            }
        }
        Ok(Self {
            ordinary,
            reasoning: reasoning.cloned(),
            versioned: None,
        })
    }

    /// Bind the existing live companion to its immutable schema sidecar.
    /// Native-v1 identity plans cannot be mixed with a v2 ordinary claim.
    pub(crate) fn new_runtime_versioned(
        ordinary: LiveClaimCommitPlan,
        claims: &[VersionedReservedTurnClaim],
        reasoning: Option<&ReasoningCoveragePlan>,
    ) -> Result<Self> {
        let versioned = VersionedLiveClaimCommitPlan::new(ordinary.clone(), claims)?;
        ensure!(
            !claims.is_empty()
                && ordinary.capture_scope.is_some()
                && matches!(ordinary.source_channel, "live" | "export"),
            "runtime coverage requires a scoped live/export claim plan"
        );
        ensure!(
            reasoning.is_none()
                || claims
                    .iter()
                    .all(|claim| claim.coverage_schema_version == CoverageSchemaVersion::V1),
            "native reasoning cannot cross coverage versions; run libra agent doctor"
        );
        let mut companion = Self::new(ordinary, reasoning)?;
        companion.versioned = Some(versioned);
        Ok(companion)
    }

    pub(crate) fn runtime_versioned(&self) -> Option<&VersionedLiveClaimCommitPlan> {
        self.versioned.as_ref()
    }
}

#[async_trait]
impl TracesTxnExtra for LiveCaptureClaimCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        match &self.reasoning {
            Some(reasoning) => {
                ReasoningClaimCommitPlan::new(self.ordinary.clone(), reasoning)?
                    .apply(txn, ctx)
                    .await
            }
            None => match &self.versioned {
                Some(versioned) => versioned.apply(txn, ctx).await,
                None => self.ordinary.apply(txn, ctx).await,
            },
        }
    }

    fn requires_versioned_coverage_binding(&self) -> bool {
        self.versioned.is_some()
    }
}

impl ReasoningClaimCommitPlan {
    pub(crate) fn new(
        ordinary: LiveClaimCommitPlan,
        reasoning: &ReasoningCoveragePlan,
    ) -> Result<Self> {
        let scope = ordinary
            .capture_scope
            .as_ref()
            .context("reasoning coverage requires the current capture scope")?;
        reasoning.validate_for(scope, &ordinary.session_id, &[])?;
        if ordinary
            .claims
            .iter()
            .any(|claim| reasoning.turn(&claim.logical_turn_key).is_none())
        {
            bail!(
                "reasoning coverage source does not match its reserved turns; run libra agent doctor"
            );
        }
        Ok(Self {
            ordinary,
            reasoning: reasoning.clone(),
        })
    }
}

#[async_trait]
impl TracesTxnExtra for ReasoningClaimCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        let scope = self
            .ordinary
            .capture_scope
            .as_ref()
            .context("reasoning coverage requires the current capture scope")?;
        self.reasoning
            .validate_for(scope, &self.ordinary.session_id, &[])?;
        scope
            .assert_workspace_fence_live(txn)
            .await
            .context("verify capture workspace lease before private reasoning commit")?;
        let reader = self.reasoning.read_context();
        let (mut records, retained_v2_entries, retained_v2_bytes) =
            match read_reasoning_map(txn, scope, &self.ordinary.session_id, Some(&reader)).await? {
                StoredReasoningMap::LegacyAbsent => (std::collections::BTreeMap::new(), 0, 0),
                StoredReasoningMap::Verified(maps) => (
                    maps.v1
                        .map_or_else(std::collections::BTreeMap::new, |map| map.records().clone()),
                    maps.v2.as_ref().map_or(0, |map| map.records().len()),
                    maps.v2_encoded_bytes,
                ),
                StoredReasoningMap::Invalid => {
                    bail!("private reasoning identity is invalid; start a new capture session")
                }
            };
        for claim in &self.ordinary.claims {
            let current = txn
                .query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ?
                   AND state = ? AND owner = ? AND fence_token = ? AND revision = ?
                   AND coverage_digest = ? AND completeness = ? LIMIT 1",
                    [
                        self.ordinary.session_id.clone().into(),
                        claim.logical_turn_key.clone().into(),
                        COVERAGE_SCHEMA_VERSION.into(),
                        reservation_state(self.ordinary.source_channel)?.into(),
                        self.ordinary.owner.clone().into(),
                        claim.fence_token.into(),
                        (claim.next_revision - 1).into(),
                        claim.coverage_digest.clone().into(),
                        claim.completeness.as_db_str().into(),
                    ],
                ))
                .await
                .context(
                    "cannot verify private reasoning claim ownership; run libra agent doctor",
                )?;
            if current.is_none() {
                bail!(
                    "reasoning coverage ownership changed; rolling back the checkpoint transaction"
                );
            }
            let identity = self.reasoning.turn(&claim.logical_turn_key)
                .context("reasoning coverage source does not match its reserved turns; run libra agent doctor")?;
            let body = ReasoningIdentityRecordBody {
                context: ReasoningIdentityRecordContext {
                    repository_id: scope.repo_id.clone(),
                    session_id: self.ordinary.session_id.clone(),
                    logical_turn_key: identity.slot().to_owned(),
                    coverage_schema_version: COVERAGE_SCHEMA_VERSION,
                    claim_revision: claim.next_revision,
                    claim_fence_token: claim.fence_token,
                    checkpoint_id: self.ordinary.checkpoint_id.clone(),
                },
                kind: identity.kind(),
                descriptor_mac: identity.descriptor_mac().to_owned(),
            };
            let bytes = ReasoningIdentityRecord::authentication_body(&body)?;
            let mac = self
                .reasoning
                .authenticate_record(txn, &bytes, None)
                .await?;
            records.insert(
                identity.slot().to_owned(),
                ReasoningIdentityRecord::from_body_and_mac(body, mac)?,
            );
        }
        // Includes the retained map, not only this attempt's new records.
        // These checks precede catalog/claim/ref-visible companion mutations.
        let bytes = ReasoningIdentitySessionMap::authentication_body(
            &scope.repo_id,
            &self.ordinary.session_id,
            &records,
        )?;
        let mac = self
            .reasoning
            .authenticate_map_for_schema_version(txn, 1, &bytes, None)
            .await?;
        let map = ReasoningIdentitySessionMap::from_records_and_mac(
            scope.repo_id.clone(),
            self.ordinary.session_id.clone(),
            records,
            mac,
        )?;
        let value = map.encode()?;
        let combined_entries = map
            .records()
            .len()
            .checked_add(retained_v2_entries)
            .context("private reasoning identity exceeds its safe capacity")?;
        let combined_bytes = value
            .len()
            .checked_add(retained_v2_bytes)
            .context("private reasoning identity exceeds its safe capacity")?;
        validate_reasoning_map_aggregate_capacity(combined_entries, combined_bytes)?;
        self.ordinary.apply(txn, ctx).await?;
        MetadataKv::set_with_conn(
            txn,
            MetadataScope::AgentReasoningIdentity,
            &self.ordinary.session_id,
            ReasoningIdentitySessionMap::KEY,
            &value,
            MetadataValueType::Text,
        )
        .await
        .context("cannot commit private reasoning identity; rolling back checkpoint transaction")?;
        Ok(())
    }
}

/// A sealed native-v2 identity companion. The plan constructor only accepts
/// payload-derived metadata, and this companion checks the exact versioned
/// source tuples before it can enter the HistoryManager ref-CAS transaction.
#[cfg_attr(not(all(test, unix)), allow(dead_code))]
pub(crate) struct NativeVersionedReasoningClaimCommitPlan {
    versioned: VersionedLiveClaimCommitPlan,
    reasoning: NativeVersionedReasoningCoveragePlan,
}

#[cfg_attr(not(all(test, unix)), allow(dead_code))]
impl NativeVersionedReasoningClaimCommitPlan {
    pub(crate) fn new(
        versioned: VersionedLiveClaimCommitPlan,
        reasoning: &NativeVersionedReasoningCoveragePlan,
    ) -> Result<Self> {
        reasoning.bind_claims(&versioned)?;
        Ok(Self {
            versioned,
            reasoning: reasoning.clone(),
        })
    }

    pub(crate) fn versioned(&self) -> &VersionedLiveClaimCommitPlan {
        &self.versioned
    }

    pub(crate) fn reasoning(&self) -> &NativeVersionedReasoningCoveragePlan {
        &self.reasoning
    }

    /// A matching coverage revision is insufficient to acknowledge a native
    /// replay: an ordinary versioned checkpoint can own that same tuple.
    /// Authenticate the retained private records against this exact sealed
    /// source. A later checkpoint may supersede the session map, in which
    /// case the older native replay fails closed rather than claiming success.
    pub(crate) async fn verify_committed_native_replay<C: ConnectionTrait>(
        &self,
        conn: &C,
    ) -> Result<bool> {
        let ordinary = self.versioned.ordinary();
        let Some(scope) = ordinary.capture_scope.as_ref() else {
            return Ok(false);
        };
        self.reasoning.bind_claims(&self.versioned)?;
        scope.assert_workspace_fence_live(conn).await?;
        let catalog = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_checkpoint
                 WHERE checkpoint_id = ? AND session_id = ? AND scope = 'committed' LIMIT 1",
                [
                    ordinary.checkpoint_id.clone().into(),
                    ordinary.session_id.clone().into(),
                ],
            ))
            .await
            .context("verify native replay checkpoint association")?;
        if catalog.is_none() {
            return Ok(false);
        }
        let reader = self.reasoning.read_context();
        let maps =
            match read_reasoning_map(conn, scope, &ordinary.session_id, Some(&reader)).await? {
                StoredReasoningMap::LegacyAbsent => return Ok(false),
                StoredReasoningMap::Invalid => {
                    bail!("private reasoning identity is invalid; run libra agent doctor")
                }
                StoredReasoningMap::Verified(maps) => maps,
            };
        for (version, claim) in self.versioned.claims() {
            let Some(identity) = self.reasoning.turn(version, &claim.logical_turn_key) else {
                return Ok(false);
            };
            let record = match version {
                CoverageSchemaVersion::V1 => maps
                    .v1
                    .as_ref()
                    .and_then(|map| map.records().get(identity.slot()))
                    .map(StoredReasoningRecord::V1),
                CoverageSchemaVersion::V2 => maps
                    .v2
                    .as_ref()
                    .and_then(|map| map.records().get(identity.slot()))
                    .map(StoredReasoningRecord::V2),
            };
            let Some(record) = record else {
                return Ok(false);
            };
            let expected = ReasoningIdentityRecordContext {
                repository_id: scope.repo_id.clone(),
                session_id: ordinary.session_id.clone(),
                logical_turn_key: identity.slot().to_owned(),
                coverage_schema_version: version.as_i64(),
                claim_revision: claim.next_revision,
                claim_fence_token: claim.fence_token,
                checkpoint_id: ordinary.checkpoint_id.clone(),
            };
            if record.validate_context(&expected).is_err()
                || record.body().kind != identity.kind()
                || record.body().descriptor_mac != identity.descriptor_mac()
            {
                return Ok(false);
            }
            let bytes = record.authentication_body()?;
            if let Err(error) = reader
                .authenticate_record_for_schema_version(
                    conn,
                    version.as_i64(),
                    &bytes,
                    Some(record.mac()),
                )
                .await
            {
                if error.is::<crate::internal::ai::capture::key::ReasoningIdentityMacMismatch>() {
                    bail!(
                        "private reasoning identity authentication failed; run libra agent doctor"
                    );
                }
                return Err(error);
            }
        }
        Ok(true)
    }

    // Only the store-owned wrapper may call this after exact marker fences
    // and replay expectations were bound. The plan itself is not a
    // TracesTxnExtra, so HistoryManager cannot accept it directly.
    pub(crate) async fn apply_store_bound(
        &self,
        _permit: &StoreBoundNativeVersionedPermit,
        txn: &DatabaseTransaction,
        ctx: &TracesCommitCtx,
    ) -> Result<()> {
        let ordinary = self.versioned.ordinary();
        let scope = ordinary
            .capture_scope
            .as_ref()
            .context("reasoning coverage requires the current capture scope")?;
        self.reasoning.bind_claims(&self.versioned)?;
        scope
            .assert_workspace_fence_live(txn)
            .await
            .context("verify capture workspace lease before private reasoning commit")?;
        let reader = self.reasoning.read_context();
        let (mut v1_records, mut v2_records, retained_v1_bytes, retained_v2_bytes) =
            match read_reasoning_map(txn, scope, &ordinary.session_id, Some(&reader)).await? {
                StoredReasoningMap::LegacyAbsent => (
                    std::collections::BTreeMap::new(),
                    std::collections::BTreeMap::new(),
                    0,
                    0,
                ),
                StoredReasoningMap::Verified(maps) => (
                    maps.v1
                        .map_or_else(std::collections::BTreeMap::new, |map| map.records().clone()),
                    maps.v2
                        .map_or_else(std::collections::BTreeMap::new, |map| map.records().clone()),
                    maps.v1_encoded_bytes,
                    maps.v2_encoded_bytes,
                ),
                StoredReasoningMap::Invalid => {
                    bail!("private reasoning identity is invalid; start a new capture session")
                }
            };
        let mut update_v1 = false;
        let mut update_v2 = false;
        let bound_claims = self.versioned.claims().collect::<Vec<_>>();
        for (version, claim) in &bound_claims {
            let previous_revision = claim
                .next_revision
                .checked_sub(1)
                .filter(|revision| *revision >= 0)
                .context("invalid coverage claim revision at private reasoning commit")?;
            let current = txn
                .query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 FROM agent_coverage_claim
                     WHERE session_id = ? AND logical_turn_key = ?
                       AND coverage_schema_version = ? AND state = ?
                       AND owner = ? AND fence_token = ? AND revision = ?
                       AND coverage_digest = ? AND completeness = ? LIMIT 1",
                    [
                        ordinary.session_id.clone().into(),
                        claim.logical_turn_key.clone().into(),
                        version.as_i64().into(),
                        reservation_state(ordinary.source_channel)?.into(),
                        ordinary.owner.clone().into(),
                        claim.fence_token.into(),
                        previous_revision.into(),
                        claim.coverage_digest.clone().into(),
                        claim.completeness.as_db_str().into(),
                    ],
                ))
                .await
                .context("verify versioned private reasoning claim ownership")?;
            ensure!(
                current.is_some(),
                "reasoning coverage ownership changed; rolling back the checkpoint transaction"
            );
            let identity = self
                .reasoning
                .turn(*version, &claim.logical_turn_key)
                .context("reasoning coverage source does not match its reserved turns; run libra agent doctor")?;
            let body = ReasoningIdentityRecordBody {
                context: ReasoningIdentityRecordContext {
                    repository_id: scope.repo_id.clone(),
                    session_id: ordinary.session_id.clone(),
                    logical_turn_key: identity.slot().to_owned(),
                    coverage_schema_version: version.as_i64(),
                    claim_revision: claim.next_revision,
                    claim_fence_token: claim.fence_token,
                    checkpoint_id: ordinary.checkpoint_id.clone(),
                },
                kind: identity.kind(),
                descriptor_mac: identity.descriptor_mac().to_owned(),
            };
            match version {
                CoverageSchemaVersion::V1 => {
                    let bytes = ReasoningIdentityRecord::authentication_body(&body)?;
                    let mac = reader
                        .authenticate_record_for_schema_version(txn, 1, &bytes, None)
                        .await?;
                    v1_records.insert(
                        identity.slot().to_owned(),
                        ReasoningIdentityRecord::from_body_and_mac(body, mac)?,
                    );
                    update_v1 = true;
                }
                CoverageSchemaVersion::V2 => {
                    let bytes = ReasoningIdentityRecordV2::authentication_body(&body)?;
                    let mac = reader
                        .authenticate_record_for_schema_version(txn, 2, &bytes, None)
                        .await?;
                    v2_records.insert(
                        identity.slot().to_owned(),
                        ReasoningIdentityRecordV2::from_body_and_mac(body, mac)?,
                    );
                    update_v2 = true;
                }
            }
        }
        let v1_value = if update_v1 {
            let bytes = ReasoningIdentitySessionMap::authentication_body(
                &scope.repo_id,
                &ordinary.session_id,
                &v1_records,
            )?;
            let mac = reader
                .authenticate_map_for_schema_version(txn, 1, &bytes, None)
                .await?;
            let map = ReasoningIdentitySessionMap::from_records_and_mac(
                scope.repo_id.clone(),
                ordinary.session_id.clone(),
                v1_records.clone(),
                mac,
            )?;
            Some(map.encode()?)
        } else {
            None
        };
        let v2_value = if update_v2 {
            let bytes = ReasoningIdentitySessionMapV2::authentication_body(
                &scope.repo_id,
                &ordinary.session_id,
                &v2_records,
            )?;
            let mac = reader
                .authenticate_map_for_schema_version(txn, 2, &bytes, None)
                .await?;
            let map = ReasoningIdentitySessionMapV2::from_records_and_mac(
                scope.repo_id.clone(),
                ordinary.session_id.clone(),
                v2_records.clone(),
                mac,
            )?;
            Some(map.encode()?)
        } else {
            None
        };
        let combined_entries = v1_records
            .len()
            .checked_add(v2_records.len())
            .context("private reasoning identity exceeds its safe capacity")?;
        let combined_bytes = v1_value
            .as_ref()
            .map_or(retained_v1_bytes, String::len)
            .checked_add(v2_value.as_ref().map_or(retained_v2_bytes, String::len))
            .context("private reasoning identity exceeds its safe capacity")?;
        validate_reasoning_map_aggregate_capacity(combined_entries, combined_bytes)?;
        ordinary.apply_bound_claims(txn, ctx, &bound_claims).await?;
        if let Some(value) = v1_value {
            MetadataKv::set_with_conn(
                txn,
                MetadataScope::AgentReasoningIdentity,
                &ordinary.session_id,
                ReasoningIdentitySessionMap::KEY,
                &value,
                MetadataValueType::Text,
            )
            .await
            .context(
                "cannot commit private reasoning identity; rolling back checkpoint transaction",
            )?;
        }
        if let Some(value) = v2_value {
            MetadataKv::set_with_conn(
                txn,
                MetadataScope::AgentReasoningIdentity,
                &ordinary.session_id,
                ReasoningIdentitySessionMapV2::KEY,
                &value,
                MetadataValueType::Text,
            )
            .await
            .context(
                "cannot commit private reasoning identity; rolling back checkpoint transaction",
            )?;
        }
        Ok(())
    }
}

/// Import-job state advanced in the same transaction as ref/catalog/claims.
#[derive(Debug, Clone)]
pub struct ImportIdentityCommit {
    pub identity_id: String,
    pub observed_digest: String,
    pub owner: String,
    pub fence_token: i64,
    pub next_ordinal: i64,
    pub final_turn: bool,
}

impl LiveClaimCommitPlan {
    async fn apply_bound_claims(
        &self,
        txn: &DatabaseTransaction,
        ctx: &TracesCommitCtx,
        bound_claims: &[(CoverageSchemaVersion, &ReservedTurnClaim)],
    ) -> Result<()> {
        ensure!(
            bound_claims.len() == self.claims.len()
                && bound_claims
                    .iter()
                    .zip(&self.claims)
                    .all(|((_, bound), ordinary)| *bound == ordinary),
            "coverage commit claim binding changed; run libra agent doctor"
        );
        if let Some(scope) = &self.capture_scope {
            scope.assert_workspace_fence_live(txn).await.context(
                "verify capture workspace lease before final coverage checkpoint transaction",
            )?;
        }
        // ADR-DR-19: tombstone is the final transactional write barrier for
        // every gated writer. It is checked after object construction but in
        // the SAME transaction as the ref CAS; an erase that wins first makes
        // this transaction fail before the ref/catalog can advance.
        let writable = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 AS writable
                 FROM agent_session s
                 WHERE s.session_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_import_tombstone t
                     WHERE t.agent_kind = s.agent_kind
                       AND t.provider_session_id = s.provider_session_id
                   )",
                [self.session_id.clone().into()],
            ))
            .await
            .context("verify agent-session tombstone write barrier")?;
        if writable.is_none() {
            bail!(
                "agent session was erased or tombstoned while the checkpoint was in flight; \
                 rolling back the checkpoint transaction"
            );
        }

        if let Some(session) = &self.import_session {
            CaptureCatalogStore::apply_import_session_lifecycle(txn, session)
                .await
                .context("commit imported session lifecycle")?;
        }

        if let Some(identity) = &self.import_identity {
            let state = if identity.final_turn {
                "committed"
            } else {
                "writing"
            };
            let committed_digest: sea_orm::Value = if identity.final_turn {
                Some(identity.observed_digest.clone()).into()
            } else {
                None::<String>.into()
            };
            let retained_owner: sea_orm::Value = if identity.final_turn {
                None::<String>.into()
            } else {
                Some(identity.owner.clone()).into()
            };
            let retained_lease: sea_orm::Value = if identity.final_turn {
                None::<i64>.into()
            } else {
                Some(
                    self.now_ms
                        .checked_add(LIVE_LEASE_MS)
                        .context("import identity lease timestamp overflow")?,
                )
                .into()
            };
            let advanced = txn
                .execute_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_import_identity
                     SET state = ?, observed_digest = ?,
                         committed_digest = COALESCE(?, committed_digest),
                         next_ordinal = ?, attempt_checkpoint_id = ?,
                         owner = ?, lease_expires_at = ?, updated_at = ?
                     WHERE identity_id = ? AND owner = ? AND fence_token = ?
                       AND state IN ('leased','writing')",
                    [
                        state.into(),
                        identity.observed_digest.clone().into(),
                        committed_digest,
                        identity.next_ordinal.into(),
                        self.checkpoint_id.clone().into(),
                        retained_owner,
                        retained_lease,
                        self.now_ms.into(),
                        identity.identity_id.clone().into(),
                        identity.owner.clone().into(),
                        identity.fence_token.into(),
                    ],
                ))
                .await
                .context("advance agent_import_identity in ref transaction")?;
            if advanced.rows_affected() != 1 {
                bail!(
                    "import identity lease was fenced out while committing turn; \
                     rolling back checkpoint transaction"
                );
            }
        }

        // Catalog row first (claim advance references checkpoint_id). The
        // checkpoint seam retains the `ON CONFLICT DO NOTHING` crash-retry
        // backstop, but runs it in this exact ref/coverage transaction.
        CheckpointCatalogMutation::new(
            self.checkpoint_id.clone(),
            self.session_id.clone(),
            CheckpointScope::Committed,
            self.parent_commit.clone(),
            self.created_at,
        )
        .context("prepare agent_checkpoint catalog companion")?
        .apply(txn, ctx)
        .await
        .context("commit typed checkpoint catalog companion in ref transaction")?;

        for (version, claim) in bound_claims {
            let previous_revision = claim
                .next_revision
                .checked_sub(1)
                .filter(|revision| *revision >= 0)
                .context("invalid coverage claim revision at commit")?;
            // Append-only revision history (ADR-DR-16).
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO agent_coverage_revision (
                    session_id, logical_turn_key, coverage_schema_version,
                    revision, checkpoint_id, coverage_digest, completeness,
                    source_channel, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                [
                    self.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                    version.as_i64().into(),
                    claim.next_revision.into(),
                    self.checkpoint_id.clone().into(),
                    claim.coverage_digest.clone().into(),
                    claim.completeness.as_db_str().into(),
                    self.source_channel.into(),
                    self.now_ms.into(),
                ],
            ))
            .await
            .context("insert agent_coverage_revision in ref transaction")?;

            // Advance the claim — owner + fence + state guarded. Zero rows
            // means our reservation was fenced out; the WHOLE transaction
            // (ref update included) must roll back (ADR-DR-10).
            let advanced = txn
                .execute_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_coverage_claim
                     SET state = 'catalog_committed', revision = ?,
                         coverage_digest = ?, completeness = ?,
                         checkpoint_id = ?, traces_commit = ?,
                         owner = NULL, lease_expires_at = NULL, updated_at = ?
                     WHERE session_id = ? AND logical_turn_key = ?
                       AND coverage_schema_version = ?
                       AND state = ?
                       AND owner = ? AND fence_token = ?
                       AND revision = ? AND coverage_digest = ? AND completeness = ?",
                    [
                        claim.next_revision.into(),
                        claim.coverage_digest.clone().into(),
                        claim.completeness.as_db_str().into(),
                        self.checkpoint_id.clone().into(),
                        ctx.commit_hash.clone().into(),
                        self.now_ms.into(),
                        self.session_id.clone().into(),
                        claim.logical_turn_key.clone().into(),
                        version.as_i64().into(),
                        reservation_state(self.source_channel)?.into(),
                        self.owner.clone().into(),
                        claim.fence_token.into(),
                        previous_revision.into(),
                        claim.coverage_digest.clone().into(),
                        claim.completeness.as_db_str().into(),
                    ],
                ))
                .await
                .context("advance agent_coverage_claim in ref transaction")?;
            if advanced.rows_affected() != 1 {
                bail!(
                    "coverage claim was fenced out during commit (stale reservation); \
                     rolling back checkpoint transaction"
                );
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TracesTxnExtra for LiveClaimCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        let bound_claims = self
            .claims
            .iter()
            .map(|claim| (CoverageSchemaVersion::V1, claim))
            .collect::<Vec<_>>();
        self.apply_bound_claims(txn, ctx, &bound_claims).await
    }
}

#[async_trait]
impl TracesTxnExtra for VersionedLiveClaimCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        let bound_claims = self.claims().collect::<Vec<_>>();
        self.ordinary
            .apply_bound_claims(txn, ctx, &bound_claims)
            .await
    }

    fn requires_versioned_coverage_binding(&self) -> bool {
        true
    }
}

fn capture_completeness(completeness: Completeness) -> CaptureCompleteness {
    match completeness {
        Completeness::Incomplete => CaptureCompleteness::Incomplete,
        Completeness::Complete => CaptureCompleteness::Complete,
    }
}

fn capture_source_channel_and_state(
    source_channel: &str,
) -> Result<(CaptureSourceChannel, CaptureCoverageClaimState)> {
    match source_channel {
        "live" => Ok((
            CaptureSourceChannel::Live,
            CaptureCoverageClaimState::ReservedLive,
        )),
        "export" => Ok((
            CaptureSourceChannel::Export,
            CaptureCoverageClaimState::ReservedLive,
        )),
        "import" => Ok((
            CaptureSourceChannel::Import,
            CaptureCoverageClaimState::ReservedImport,
        )),
        other => bail!("unsupported coverage source channel '{other}'"),
    }
}

/// Fetch one `agent_coverage_claim` row via [`CaptureStore`] (`RB-22`).
pub async fn get_coverage_claim_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: CoverageSchemaVersion,
) -> Result<Option<AgentCoverageClaimRecord>> {
    capture
        .get_coverage_claim(
            session_id,
            logical_turn_key,
            coverage_schema_version.as_i64(),
        )
        .await
        .context("query agent_coverage_claim from capture store")
}

/// Fetch one `agent_coverage_conflict` row via [`CaptureStore`] (`RB-22`).
pub async fn get_coverage_conflict_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: CoverageSchemaVersion,
) -> Result<Option<AgentCoverageConflictRecord>> {
    capture
        .get_coverage_conflict(
            session_id,
            logical_turn_key,
            coverage_schema_version.as_i64(),
        )
        .await
        .context("query agent_coverage_conflict from capture store")
}

/// List all `agent_coverage_revision` rows for a turn via [`CaptureStore`] (`RB-22`).
pub async fn list_coverage_revisions_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: CoverageSchemaVersion,
) -> Result<Vec<AgentCoverageRevisionRecord>> {
    capture
        .list_coverage_revisions(
            session_id,
            logical_turn_key,
            coverage_schema_version.as_i64(),
        )
        .await
        .context("list agent_coverage_revision rows from capture store")
}

/// Reserve turn coverage claims on [`CaptureStore`] following ADR-DR-09
/// arbitration (fresh insert, equivalent no-op, incomplete->complete upgrade,
/// complete-vs-different-complete conflict persistence, live lease skip, and
/// expired-lease takeover) (`RB-22`).
pub async fn reserve_turn_claims_for_channel_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<LiveReservationOutcome> {
    let (channel, reserved_state) = capture_source_channel_and_state(source_channel)?;
    let Some(session) = capture
        .get_session(session_id)
        .await
        .context("verify session availability for coverage reservation in capture store")?
    else {
        bail!("agent session was erased or is unavailable for coverage reservation");
    };
    let tombstones = capture
        .list_import_tombstones()
        .await
        .context("verify coverage reservation tombstone barrier in capture store")?;
    if tombstones.iter().any(|t| {
        t.agent_kind == session.agent_kind.as_str()
            && t.provider_session_id == session.provider_session_id
    }) {
        bail!("agent session was erased or is unavailable for coverage reservation");
    }

    let mut outcome = LiveReservationOutcome::default();
    let lease_expires_at = lease_deadline(now_ms)?;

    for turn in turns {
        let encoded = encode_coverage_turn_for_schema(COVERAGE_SCHEMA_VERSION, turn)?;
        let digest = encoded.digest_hex;
        let incoming_completeness = capture_completeness(turn.completeness);
        let existing = capture
            .get_coverage_claim(session_id, &turn.logical_turn_key, COVERAGE_SCHEMA_VERSION)
            .await
            .context("read existing agent_coverage_claim from capture store")?;

        match existing {
            None => {
                let record = AgentCoverageClaimRecord {
                    session_id: session_id.to_string(),
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_schema_version: COVERAGE_SCHEMA_VERSION,
                    coverage_digest: digest.clone(),
                    completeness: incoming_completeness,
                    revision: 0,
                    state: reserved_state,
                    attempt_checkpoint_id: None,
                    owner: Some(owner.to_string()),
                    lease_expires_at: Some(lease_expires_at),
                    fence_token: Some(1),
                    checkpoint_id: None,
                    traces_commit: None,
                    source_channel: channel,
                    created_at: now_ms,
                    updated_at: now_ms,
                };
                capture
                    .upsert_coverage_claim(&record)
                    .await
                    .context("insert agent_coverage_claim reservation in capture store")?;
                outcome.reserved.push(ReservedTurnClaim {
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_digest: digest,
                    completeness: turn.completeness,
                    fence_token: 1,
                    next_revision: 1,
                });
            }
            Some(mut incumbent) => match incumbent.state {
                CaptureCoverageClaimState::CatalogCommitted => {
                    let completeness_upgrade = incumbent.completeness
                        == CaptureCompleteness::Incomplete
                        && incoming_completeness == CaptureCompleteness::Complete;
                    if incumbent.coverage_digest == digest && !completeness_upgrade {
                        outcome.skipped_covered += 1;
                    } else if completeness_upgrade {
                        let new_fence = incumbent
                            .fence_token
                            .unwrap_or(0)
                            .checked_add(1)
                            .context("coverage claim fence token overflow")?;
                        let next_rev = next_revision(incumbent.revision)?;
                        incumbent.state = reserved_state;
                        incumbent.owner = Some(owner.to_string());
                        incumbent.lease_expires_at = Some(lease_expires_at);
                        incumbent.fence_token = Some(new_fence);
                        incumbent.coverage_digest = digest.clone();
                        incumbent.completeness = incoming_completeness;
                        incumbent.source_channel = channel;
                        incumbent.updated_at = now_ms;
                        capture
                            .upsert_coverage_claim(&incumbent)
                            .await
                            .context("upgrade incomplete coverage claim in capture store")?;
                        outcome.reserved.push(ReservedTurnClaim {
                            logical_turn_key: turn.logical_turn_key.clone(),
                            coverage_digest: digest,
                            completeness: turn.completeness,
                            fence_token: new_fence,
                            next_revision: next_rev,
                        });
                    } else if incumbent.completeness == CaptureCompleteness::Complete
                        && incoming_completeness == CaptureCompleteness::Incomplete
                    {
                        outcome.skipped_covered += 1;
                    } else if incumbent.completeness == CaptureCompleteness::Complete
                        && incoming_completeness == CaptureCompleteness::Complete
                    {
                        let mut sanitized_turn = turn.clone();
                        let redaction_report =
                            redact_turns_with_report(std::slice::from_mut(&mut sanitized_turn));
                        let sanitized_encoded = encode_coverage_turn_for_schema(
                            COVERAGE_SCHEMA_VERSION,
                            &sanitized_turn,
                        )?;
                        if sanitized_encoded.digest_hex == incumbent.coverage_digest {
                            outcome.skipped_covered += 1;
                        } else {
                            let incoming_canonical_json =
                                String::from_utf8(sanitized_encoded.canonical_bytes)
                                    .context("coverage conflict canonical evidence is not UTF-8")?;
                            let incoming_redaction_report_json =
                                serde_json::to_string(&redaction_report)
                                    .context("serialize coverage conflict redaction report")?;
                            let incumbent_revision = incumbent.revision;
                            let incumbent_digest = incumbent.coverage_digest.clone();
                            let incumbent_checkpoint_id = incumbent.checkpoint_id.clone();
                            incumbent.state = CaptureCoverageClaimState::Conflicted;
                            incumbent.updated_at = now_ms;
                            capture
                                .upsert_coverage_claim(&incumbent)
                                .await
                                .context("mark agent_coverage_claim conflicted in capture store")?;
                            if capture
                                .get_coverage_conflict(
                                    session_id,
                                    &turn.logical_turn_key,
                                    COVERAGE_SCHEMA_VERSION,
                                )
                                .await
                                .context("check existing coverage conflict in capture store")?
                                .is_none()
                            {
                                let conflict = AgentCoverageConflictRecord {
                                    session_id: session_id.to_string(),
                                    logical_turn_key: turn.logical_turn_key.clone(),
                                    coverage_schema_version: COVERAGE_SCHEMA_VERSION,
                                    incumbent_revision,
                                    incumbent_digest,
                                    incumbent_checkpoint_id,
                                    incoming_digest: sanitized_encoded.digest_hex,
                                    incoming_source_channel: channel,
                                    incoming_observed_at: now_ms,
                                    incoming_canonical_json,
                                    incoming_redaction_report_json,
                                };
                                capture.upsert_coverage_conflict(&conflict).await.context(
                                    "persist sanitized coverage conflict challenger in capture store",
                                )?;
                            }
                            outcome.conflicted += 1;
                        }
                    } else {
                        // Incomplete vs different Incomplete: advance revision on new incomplete snapshot.
                        let new_fence = incumbent
                            .fence_token
                            .unwrap_or(0)
                            .checked_add(1)
                            .context("coverage claim fence token overflow")?;
                        let next_rev = next_revision(incumbent.revision)?;
                        incumbent.state = reserved_state;
                        incumbent.owner = Some(owner.to_string());
                        incumbent.lease_expires_at = Some(lease_expires_at);
                        incumbent.fence_token = Some(new_fence);
                        incumbent.coverage_digest = digest.clone();
                        incumbent.completeness = incoming_completeness;
                        incumbent.source_channel = channel;
                        incumbent.updated_at = now_ms;
                        capture
                            .upsert_coverage_claim(&incumbent)
                            .await
                            .context("re-own incomplete coverage claim in capture store")?;
                        outcome.reserved.push(ReservedTurnClaim {
                            logical_turn_key: turn.logical_turn_key.clone(),
                            coverage_digest: digest,
                            completeness: turn.completeness,
                            fence_token: new_fence,
                            next_revision: next_rev,
                        });
                    }
                }
                CaptureCoverageClaimState::Conflicted => {
                    outcome.conflicted += 1;
                }
                CaptureCoverageClaimState::ReservedLive
                | CaptureCoverageClaimState::ReservedImport => {
                    if incumbent.lease_expires_at == Some(ARTIFACT_RETAINED_LEASE_MS) {
                        outcome.skipped_inflight += 1;
                        outcome.skipped_retained += 1;
                    } else if incumbent.lease_expires_at.is_some_and(|exp| exp > now_ms) {
                        outcome.skipped_inflight += 1;
                    } else {
                        let new_fence = incumbent
                            .fence_token
                            .unwrap_or(0)
                            .checked_add(1)
                            .context("coverage claim fence token overflow")?;
                        let next_rev = next_revision(incumbent.revision)?;
                        incumbent.state = reserved_state;
                        incumbent.owner = Some(owner.to_string());
                        incumbent.lease_expires_at = Some(lease_expires_at);
                        incumbent.fence_token = Some(new_fence);
                        incumbent.coverage_digest = digest.clone();
                        incumbent.completeness = incoming_completeness;
                        incumbent.source_channel = channel;
                        incumbent.updated_at = now_ms;
                        capture
                            .upsert_coverage_claim(&incumbent)
                            .await
                            .context("take over expired coverage claim in capture store")?;
                        outcome.reserved.push(ReservedTurnClaim {
                            logical_turn_key: turn.logical_turn_key.clone(),
                            coverage_digest: digest,
                            completeness: turn.completeness,
                            fence_token: new_fence,
                            next_revision: next_rev,
                        });
                    }
                }
                CaptureCoverageClaimState::Abandoned => {
                    let new_fence = incumbent
                        .fence_token
                        .unwrap_or(0)
                        .checked_add(1)
                        .context("coverage claim fence token overflow")?;
                    let next_rev = next_revision(incumbent.revision)?;
                    incumbent.state = reserved_state;
                    incumbent.owner = Some(owner.to_string());
                    incumbent.lease_expires_at = Some(lease_expires_at);
                    incumbent.fence_token = Some(new_fence);
                    incumbent.coverage_digest = digest.clone();
                    incumbent.completeness = incoming_completeness;
                    incumbent.source_channel = channel;
                    incumbent.updated_at = now_ms;
                    capture
                        .upsert_coverage_claim(&incumbent)
                        .await
                        .context("re-reserve abandoned coverage claim in capture store")?;
                    outcome.reserved.push(ReservedTurnClaim {
                        logical_turn_key: turn.logical_turn_key.clone(),
                        coverage_digest: digest,
                        completeness: turn.completeness,
                        fence_token: new_fence,
                        next_revision: next_rev,
                    });
                }
            },
        }
    }

    Ok(outcome)
}

/// Commit reserved coverage claims into `agent_coverage_revision` and advance
/// `agent_coverage_claim` to `catalog_committed` via [`CaptureStore`] (`RB-22`).
#[allow(clippy::too_many_arguments)]
pub async fn commit_reserved_turn_claims_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    owner: &str,
    checkpoint_id: &str,
    traces_commit: &str,
    source_channel: &'static str,
    claims: &[ReservedTurnClaim],
    now_ms: i64,
) -> Result<()> {
    let (channel, expected_state) = capture_source_channel_and_state(source_channel)?;
    for claim in claims {
        let prev_revision = claim
            .next_revision
            .checked_sub(1)
            .context("invalid reserved turn claim next_revision")?;
        let mut current = capture
            .get_coverage_claim(session_id, &claim.logical_turn_key, COVERAGE_SCHEMA_VERSION)
            .await
            .context("read reserved coverage claim before commit")?
            .context("reserved coverage claim is missing before commit")?;
        ensure!(
            current.state == expected_state
                && current.owner.as_deref() == Some(owner)
                && current.fence_token == Some(claim.fence_token)
                && current.revision == prev_revision,
            "coverage claim ownership fence changed before commit for turn '{}'",
            claim.logical_turn_key
        );
        let revision = AgentCoverageRevisionRecord {
            session_id: session_id.to_string(),
            logical_turn_key: claim.logical_turn_key.clone(),
            coverage_schema_version: COVERAGE_SCHEMA_VERSION,
            revision: claim.next_revision,
            checkpoint_id: checkpoint_id.to_string(),
            coverage_digest: claim.coverage_digest.clone(),
            completeness: capture_completeness(claim.completeness),
            source_channel: channel,
            created_at: now_ms,
        };
        capture
            .put_coverage_revision(&revision)
            .await
            .context("insert agent_coverage_revision in capture store")?;

        current.state = CaptureCoverageClaimState::CatalogCommitted;
        current.revision = claim.next_revision;
        current.coverage_digest = claim.coverage_digest.clone();
        current.completeness = capture_completeness(claim.completeness);
        current.checkpoint_id = Some(checkpoint_id.to_string());
        current.traces_commit = Some(traces_commit.to_string());
        current.owner = None;
        current.lease_expires_at = None;
        current.attempt_checkpoint_id = None;
        current.updated_at = now_ms;
        capture
            .upsert_coverage_claim(&current)
            .await
            .context("commit agent_coverage_claim in capture store")?;
    }
    Ok(())
}

/// Abandon still-owned uncommitted reservations via [`CaptureStore`] (`RB-22`).
pub async fn abandon_reserved_turn_claims_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    claims: &[ReservedTurnClaim],
    now_ms: i64,
) -> Result<usize> {
    let (_, expected_state) = capture_source_channel_and_state(source_channel)?;
    let mut abandoned = 0usize;
    for claim in claims {
        let Some(mut current) = capture
            .get_coverage_claim(session_id, &claim.logical_turn_key, COVERAGE_SCHEMA_VERSION)
            .await
            .context("read reserved coverage claim for abandonment")?
        else {
            continue;
        };
        if current.state == expected_state
            && current.owner.as_deref() == Some(owner)
            && current.fence_token == Some(claim.fence_token)
        {
            current.state = CaptureCoverageClaimState::Abandoned;
            current.owner = None;
            current.lease_expires_at = None;
            current.attempt_checkpoint_id = None;
            current.fence_token = Some(current.fence_token.unwrap_or(0).saturating_add(1));
            current.updated_at = now_ms;
            capture
                .upsert_coverage_claim(&current)
                .await
                .context("abandon reserved coverage claim in capture store")?;
            abandoned += 1;
        }
    }
    Ok(abandoned)
}

#[cfg(test)]
pub(crate) mod tests {
    #[cfg(unix)]
    use chrono::Utc;
    use sea_orm::{Database, TransactionTrait};

    use super::*;
    use crate::internal::{
        ai::observed_agents::{SemanticRecord, canonical_turn_bytes},
        config::ConfigKv,
        db::migration::run_builtin_migrations,
    };

    #[tokio::test]
    async fn cov11_import_history_preflight_before_session_exists_is_read_only() {
        let (conn, scope) = scoped_gate_db().await;
        let fresh_session = "opencode__fresh-import-before-identity";
        let existing_session = "claude_code__scope_expiry";
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap();
        let visible = cov2_visible_turn(
            "history-turn",
            "ordinary visible data",
            Completeness::Complete,
        );
        let history = reserve_turn_claims_by_version_with_capture_scope(
            &conn,
            &scope,
            existing_session,
            &[visible],
            &[2],
            "history-owner",
            1,
            "live",
        )
        .await
        .unwrap();
        assert_eq!(history.reserved.len(), 1);
        // SQLite rejects writes while these calls execute; total_changes also
        // pins that none of the successful or rejected preflights mutated it.
        conn.execute_unprepared("PRAGMA query_only = ON")
            .await
            .unwrap();
        let before = scalar(&conn, "SELECT total_changes() AS n").await;
        for rejected in [false, true] {
            ensure_runtime_import_source_history_until(
                &conn,
                &scope,
                fresh_session,
                deadline,
                rejected,
            )
            .await
            .expect("fresh imports must not require a preexisting session row");
        }
        let fresh_count = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = ?",
                [fresh_session.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by::<i64, _>("n")
            .unwrap();
        assert_eq!(
            fresh_count, 0,
            "history preflight must not materialize the session"
        );
        let error = ensure_runtime_import_source_history_until(
            &conn,
            &scope,
            existing_session,
            deadline,
            true,
        )
        .await
        .expect_err("existing v2 history must reject the whole rejected-source attempt");
        assert!(error.to_string().contains("start a new session"));
        ensure_runtime_import_source_history_until(
            &conn,
            &scope,
            existing_session,
            deadline,
            false,
        )
        .await
        .expect("non-rejected sources retain their availability contract");
        let wrong_scope = CaptureScope {
            workspace_fence: scope.workspace_fence.map(|fence| fence + 1),
            ..scope.clone()
        };
        assert!(
            ensure_runtime_import_source_history_until(
                &conn,
                &wrong_scope,
                fresh_session,
                deadline,
                false,
            )
            .await
            .is_err(),
            "a missing session must not bypass the workspace fence"
        );
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_millis(1), 0);
        let error =
            ensure_runtime_import_source_history_until(&conn, &scope, fresh_session, expired, true)
                .await
                .expect_err("expired preflight must fail before history classification");
        assert!(error.to_string().contains("deadline"));
        assert_eq!(scalar(&conn, "SELECT total_changes() AS n").await, before);
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
            1
        );
        conn.execute_unprepared("PRAGMA query_only = OFF")
            .await
            .unwrap();
        conn.execute_unprepared("DROP INDEX idx_agent_coverage_claim_logical_key")
            .await
            .unwrap();
        conn.execute_unprepared("PRAGMA query_only = ON")
            .await
            .unwrap();
        let before_error = scalar(&conn, "SELECT total_changes() AS n").await;
        let error = ensure_runtime_import_source_history_until(
            &conn,
            &scope,
            fresh_session,
            deadline,
            true,
        )
        .await
        .expect_err("database errors must not look like absent history");
        assert!(format!("{error:#}").contains("cannot check rejected capture coverage history"));
        assert_eq!(
            scalar(&conn, "SELECT total_changes() AS n").await,
            before_error
        );
    }

    #[cfg(unix)]
    async fn cov11_runtime_reserve(
        fixture: &ReasoningFixture,
        turns: &[NormalizedTurn],
        rejected: bool,
    ) -> Result<ReasoningReservationOutcome> {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture_ids;
        reserve_runtime_turn_claims_with_reasoning(
            &fixture.conn,
            &fixture.scope,
            fixture_ids::CAPTURE,
            turns,
            "runtime-owner",
            1,
            CaptureReservationExecution::live(CaptureCommitDeadline::from_budget(
                Duration::from_secs(30),
            )?),
            ReasoningCoverageInput::ordinary(&fixture.reader()),
            rejected,
        )
        .await
    }

    #[cfg(unix)]
    async fn cov11_commit_runtime(
        fixture: &ReasoningFixture,
        claims: &[VersionedReservedTurnClaim],
        checkpoint_id: &str,
    ) {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture_ids;
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: fixture_ids::CAPTURE.to_owned(),
            checkpoint_id: checkpoint_id.to_owned(),
            owner: "runtime-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: claims.iter().map(|claim| claim.claim.clone()).collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let plan = VersionedLiveClaimCommitPlan::new(ordinary, claims).unwrap();
        let txn = fixture.conn.begin().await.unwrap();
        plan.apply(
            &txn,
            &TracesCommitCtx {
                commit_hash: "d".repeat(40),
                tree_oid: "b".repeat(40),
                metadata_blob_oid: "c".repeat(40),
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov11_runtime_selection_rejected_history_and_no_demotion() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture_ids;
        let fixture = ReasoningFixture::new().await;
        let turns = [
            turn("ordinary", "legacy", Completeness::Complete),
            cov2_visible_turn("visible", "redacted typed text", Completeness::Complete),
        ];
        let reserved = cov11_runtime_reserve(&fixture, &turns, false)
            .await
            .unwrap();
        assert_eq!(
            reserved
                .versioned_reserved
                .iter()
                .map(|claim| claim.coverage_schema_version.as_i64())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            reserved.versioned_reserved[0].claim.coverage_digest,
            encode_coverage_turn_for_schema(1, &turns[0])
                .unwrap()
                .digest_hex
        );
        cov11_commit_runtime(&fixture, &reserved.versioned_reserved, "runtime-checkpoint").await;
        let covered = cov11_runtime_reserve(&fixture, &turns, false)
            .await
            .unwrap();
        assert_eq!(covered.ordinary.skipped_covered, 2);
        assert_eq!(covered.covered.len(), 2);
        assert_eq!(covered.covered[1].version(), CoverageSchemaVersion::V2);
        let older_incomplete = cov11_runtime_reserve(
            &fixture,
            &[cov2_visible_turn(
                "visible",
                "a different incomplete parse",
                Completeness::Incomplete,
            )],
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            older_incomplete.covered[0].coverage_digest,
            reserved.versioned_reserved[1].claim.coverage_digest
        );
        assert_eq!(
            older_incomplete.covered[0].completeness,
            Completeness::Complete,
            "covered replay must retain actual committed content, never the incoming candidate"
        );
        for attempted in [
            vec![],
            vec![turn("unrelated-new", "ordinary", Completeness::Complete)],
        ] {
            let error = cov11_runtime_reserve(&fixture, &attempted, true)
                .await
                .err()
                .unwrap();
            assert!(error.to_string().contains("start a new session"));
        }
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            2
        );
        assert!(
            cov11_runtime_reserve(
                &fixture,
                &[turn("visible", "fallback", Completeness::Complete)],
                false
            )
            .await
            .is_err()
        );
        let fresh = [turn("new-valid", "ordinary", Completeness::Complete)];
        let accepted = cov11_runtime_reserve(&fixture, &fresh, false)
            .await
            .unwrap();
        assert_eq!(
            accepted.versioned_reserved[0].coverage_schema_version,
            CoverageSchemaVersion::V1
        );
        // Fresh rejected sources retain the exact ordinary-v1 route when no
        // v2 history exists, including its old digest and error contract.
        let legacy = ReasoningFixture::new().await;
        let accepted = cov11_runtime_reserve(&legacy, &fresh, true).await.unwrap();
        assert_eq!(
            accepted.versioned_reserved[0].coverage_schema_version,
            CoverageSchemaVersion::V1
        );
        assert_eq!(
            accepted.versioned_reserved[0].claim.coverage_digest,
            encode_coverage_turn_for_schema(1, &fresh[0])
                .unwrap()
                .digest_hex
        );
        fixture
            .conn
            .execute_unprepared("DROP INDEX idx_agent_coverage_claim_logical_key")
            .await
            .unwrap();
        let error = reserve_runtime_import_turn_claims_with_reasoning_until(
            &fixture.conn,
            &fixture.scope,
            fixture_ids::CAPTURE,
            &[],
            "import-owner",
            1,
            CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
            ReasoningCoverageInput::ordinary(&fixture.reader()),
            true,
        )
        .await
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("cannot check rejected capture coverage history"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov11_gate_owned_exact_covered_replay_fails_closed() {
        use crate::internal::ai::{
            capture::checkpoint::{
                CheckpointReplayStatus, CheckpointStoreError, TracesCheckpointStore,
            },
            observed_agents::reasoning::carrier_test_support as fixture_ids,
        };
        let fixture = ReasoningFixture::new().await;
        let turns = [
            turn("ordinary", "legacy", Completeness::Complete),
            cov2_visible_turn("visible", "redacted typed text", Completeness::Complete),
        ];
        let reserved = cov11_runtime_reserve(&fixture, &turns, false)
            .await
            .unwrap();
        cov11_commit_runtime(&fixture, &reserved.versioned_reserved, "runtime-checkpoint").await;
        let mut covered = cov11_runtime_reserve(&fixture, &turns, false)
            .await
            .unwrap();
        async fn settle(
            fixture: &ReasoningFixture,
            claims: &[CommittedTurnClaimSnapshot],
        ) -> Result<CheckpointReplayStatus, CheckpointStoreError> {
            TracesCheckpointStore::settle_runtime_covered_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                "runtime-checkpoint",
                fixture_ids::CAPTURE,
                None,
                claims,
            )
            .await
        }
        assert_eq!(
            settle(&fixture, &covered.covered).await,
            Ok(CheckpointReplayStatus::Complete)
        );
        assert_eq!(
            settle(&fixture, &covered.covered).await,
            Ok(CheckpointReplayStatus::Complete)
        );
        covered.covered[1].requires_native_replay = true;
        assert_eq!(
            settle(&fixture, &covered.covered).await,
            Err(CheckpointStoreError::ReplayPayloadMismatch),
            "an ordinary exact set cannot waive authenticated native replay"
        );
        covered.covered[1].requires_native_replay = false;
        assert_eq!(
            settle(&fixture, &covered.covered[1..]).await,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        );
        assert_eq!(
            settle(&fixture, &covered.covered[..1]).await,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        );
        assert_eq!(
            settle(&fixture, &[]).await,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        );
        assert_eq!(
            TracesCheckpointStore::settle_runtime_covered_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                "foreign-action",
                fixture_ids::CAPTURE,
                None,
                &covered.covered,
            )
            .await,
            Ok(CheckpointReplayStatus::Missing)
        );
        assert!(
            !TracesCheckpointStore::durable_legacy_replay_exists_for(
                &fixture.conn,
                &fixture.scope,
                "runtime-checkpoint",
                fixture_ids::CAPTURE,
                None,
            )
            .await
            .unwrap()
        );
        fixture.conn.execute_unprepared("UPDATE agent_coverage_claim SET state = 'conflicted' WHERE coverage_schema_version = 2").await.unwrap();
        assert_eq!(
            settle(&fixture, &covered.covered).await,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision"
            )
            .await,
            2
        );
        fixture.conn.execute_unprepared("UPDATE agent_coverage_claim SET state = 'catalog_committed' WHERE coverage_schema_version = 2").await.unwrap();
        fixture
            .conn
            .execute_unprepared(
                "DELETE FROM agent_checkpoint WHERE checkpoint_id = 'runtime-checkpoint'",
            )
            .await
            .unwrap();
        assert_eq!(
            settle(&fixture, &covered.covered).await,
            Err(CheckpointStoreError::ReplayPayloadMismatch),
            "gate-owned v2 expectations cannot settle a missing durable checkpoint"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov11_runtime_capacity_and_expired_deadline_reject_before_write() {
        let fixture = ReasoningFixture::new().await;
        let max_entries = crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES;
        let max_bytes = crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES;
        assert!(validate_reasoning_map_aggregate_capacity(max_entries, max_bytes).is_ok());
        assert!(validate_reasoning_map_aggregate_capacity(max_entries + 1, max_bytes).is_err());
        assert!(validate_reasoning_map_aggregate_capacity(max_entries, max_bytes + 1).is_err());
        let turns = (0..=max_entries)
            .map(|index| {
                cov2_visible_turn(
                    &format!("visible-{index}"),
                    "redacted",
                    Completeness::Complete,
                )
            })
            .collect::<Vec<_>>();
        let error = cov11_runtime_reserve(&fixture, &turns, false)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("safe limit"));
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_millis(1), 0);
        let error = reserve_runtime_turn_claims_with_reasoning(
            &fixture.conn,
            &fixture.scope,
            crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
            &turns[..1],
            "runtime-owner",
            1,
            CaptureReservationExecution::live(expired),
            ReasoningCoverageInput::ordinary(&fixture.reader()),
            false,
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("deadline"));
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0
        );
        let oversized = ReasoningFixture::new().await;
        let v1 = cov2_seed_bulk_signed_private_map(&oversized, 1, 1500).await;
        let v2 = cov2_seed_bulk_signed_private_map(&oversized, 2, 1500).await;
        assert!(v1.len() <= max_bytes && v2.len() <= max_bytes && v1.len() + v2.len() > max_bytes);
        assert!(
            cov11_runtime_reserve(&oversized, &turns[..1], false)
                .await
                .is_err()
        );
        assert_eq!(
            scalar(
                &oversized.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0,
            "combined map byte capacity must fail closed before any runtime claim"
        );
    }

    pub(crate) fn cov2_visible_turn(
        key: &str,
        text: &str,
        completeness: Completeness,
    ) -> NormalizedTurn {
        use crate::internal::ai::observed_agents::reasoning::{
            ReasoningProvider, ReasoningSourceKind, classify_provider_visible_reasoning,
        };
        let (_, visible) = classify_provider_visible_reasoning(
            ReasoningProvider::OpenCode,
            ReasoningSourceKind::Reasoning,
            text.to_owned(),
        );
        let mut turn = turn(key, "same ordinary prompt", completeness);
        turn.records.push(SemanticRecord::Reasoning {
            provider: ReasoningProvider::OpenCode,
            source_kind: Some(ReasoningSourceKind::Reasoning),
            text: visible,
        });
        turn
    }

    #[cfg(unix)]
    pub(crate) fn cov2_native_v2_turn(
        key: &str,
        text: &str,
        completeness: Completeness,
    ) -> NormalizedTurn {
        use crate::internal::ai::observed_agents::reasoning::{
            ReasoningProvider, ReasoningSourceKind, classify_provider_visible_reasoning,
        };
        let (_, visible) = classify_provider_visible_reasoning(
            ReasoningProvider::ClaudeCode,
            ReasoningSourceKind::Reasoning,
            text.to_owned(),
        );
        let mut turn = turn(key, "same ordinary prompt", completeness);
        turn.records.push(SemanticRecord::Reasoning {
            provider: ReasoningProvider::ClaudeCode,
            source_kind: Some(ReasoningSourceKind::Reasoning),
            text: visible,
        });
        turn
    }

    async fn cov2_mark_claim_committed(
        conn: &DatabaseConnection,
        bound: &VersionedReservedTurnClaim,
        owner: &str,
        checkpoint_id: &str,
    ) {
        let changed = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_coverage_claim
                 SET state = 'catalog_committed', revision = ?, owner = NULL,
                     lease_expires_at = NULL, checkpoint_id = ?
                 WHERE session_id = ? AND logical_turn_key = ?
                   AND coverage_schema_version = ? AND state = 'reserved_live'
                   AND owner = ? AND fence_token = ?",
                [
                    bound.claim.next_revision.into(),
                    checkpoint_id.into(),
                    "claude_code__s1".into(),
                    bound.claim.logical_turn_key.clone().into(),
                    bound.coverage_schema_version.as_i64().into(),
                    owner.into(),
                    bound.claim.fence_token.into(),
                ],
            ))
            .await
            .expect("commit synthetic DB claim");
        assert_eq!(changed.rows_affected(), 1);
    }

    async fn cov2_versioned_verify_fixture() -> (
        DatabaseConnection,
        CaptureScope,
        VersionedLiveClaimCommitPlan,
    ) {
        let conn = gate_db().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("capture scope");
        let turns = [
            turn("cov2-verify-shared", "ordinary", Completeness::Complete),
            cov2_visible_turn("cov2-verify-shared", "visible", Completeness::Complete),
        ];
        let reserved = reserve_turn_claims_by_version_with_capture_scope(
            &conn,
            &scope,
            "claude_code__s1",
            &turns,
            &[1, 2],
            "verify-owner",
            1,
            "live",
        )
        .await
        .expect("reserve both schema versions of one key");
        assert_eq!(reserved.reserved.len(), 2);
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: "claude_code__s1".to_owned(),
            checkpoint_id: "cov2-verify-checkpoint".to_owned(),
            owner: "verify-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: reserved
                .reserved
                .iter()
                .map(|bound| bound.claim.clone())
                .collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(scope.clone()),
        };
        let plan = VersionedLiveClaimCommitPlan::new(ordinary, &reserved.reserved)
            .expect("bind both real DB reservations");
        (conn, scope, plan)
    }

    fn cov2_verify_subset_plan(
        plan: &VersionedLiveClaimCommitPlan,
        index: usize,
    ) -> VersionedLiveClaimCommitPlan {
        let (coverage_schema_version, claim) = plan
            .claims()
            .nth(index)
            .expect("the fixture has two versions");
        let bound = VersionedReservedTurnClaim {
            coverage_schema_version,
            claim: claim.clone(),
        };
        let mut ordinary = plan.ordinary().clone();
        ordinary.claims = vec![bound.claim.clone()];
        VersionedLiveClaimCommitPlan::new(ordinary, &[bound])
            .expect("bind one schema version independently")
    }

    async fn cov2_verify_claim_rows(conn: &impl ConnectionTrait) -> Vec<String> {
        let mut rows = Vec::with_capacity(2);
        for version in [1_i64, 2_i64] {
            let row = conn
                .query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT logical_turn_key, coverage_schema_version, coverage_digest,
                            completeness, revision, state, attempt_checkpoint_id,
                            owner, lease_expires_at, fence_token, checkpoint_id,
                            traces_commit, source_channel, created_at, updated_at
                     FROM agent_coverage_claim
                     WHERE session_id = 'claude_code__s1'
                       AND logical_turn_key = 'cov2-verify-shared'
                       AND coverage_schema_version = ?",
                    [version.into()],
                ))
                .await
                .expect("read original versioned claim")
                .expect("original versioned claim exists");
            rows.push(format!(
                "{:?}{:?}",
                (
                    row.try_get_by::<String, _>("logical_turn_key")
                        .expect("key"),
                    row.try_get_by::<i64, _>("coverage_schema_version")
                        .expect("version"),
                    row.try_get_by::<String, _>("coverage_digest")
                        .expect("digest"),
                    row.try_get_by::<String, _>("completeness")
                        .expect("completeness"),
                    row.try_get_by::<i64, _>("revision").expect("revision"),
                    row.try_get_by::<String, _>("state").expect("state"),
                    row.try_get_by::<Option<String>, _>("attempt_checkpoint_id")
                        .expect("attempt checkpoint"),
                    row.try_get_by::<Option<String>, _>("owner").expect("owner"),
                ),
                (
                    row.try_get_by::<Option<i64>, _>("lease_expires_at")
                        .expect("lease"),
                    row.try_get_by::<Option<i64>, _>("fence_token")
                        .expect("fence"),
                    row.try_get_by::<Option<String>, _>("checkpoint_id")
                        .expect("checkpoint"),
                    row.try_get_by::<Option<String>, _>("traces_commit")
                        .expect("traces commit"),
                    row.try_get_by::<String, _>("source_channel")
                        .expect("source channel"),
                    row.try_get_by::<i64, _>("created_at").expect("created at"),
                    row.try_get_by::<i64, _>("updated_at").expect("updated at"),
                )
            ));
        }
        rows
    }

    #[tokio::test]
    async fn cov2_versioned_reserved_live_claims_verify_actual_tuple() {
        let (conn, scope, plan) = cov2_versioned_verify_fixture().await;
        let original = cov2_verify_claim_rows(&conn).await;
        verify_versioned_reserved_live_claims_with_conn(&conn, &scope, &plan)
            .await
            .expect("both actual version-keyed DB tuples match their sealed claims");
        let mut empty_ordinary = plan.ordinary().clone();
        empty_ordinary.claims.clear();
        let empty = VersionedLiveClaimCommitPlan::new(empty_ordinary, &[])
            .expect("an empty version sidecar is structurally valid");
        assert!(
            verify_versioned_reserved_live_claims_with_conn(&conn, &scope, &empty)
                .await
                .is_err(),
            "an empty pending claim set must not receive a vacuous ownership ACK"
        );
        let excessive_bound = (0
            ..=crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES)
            .map(|index| VersionedReservedTurnClaim {
                coverage_schema_version: CoverageSchemaVersion::V1,
                claim: ReservedTurnClaim {
                    logical_turn_key: format!("excessive-{index}"),
                    coverage_digest: "0".repeat(64),
                    completeness: Completeness::Complete,
                    fence_token: 1,
                    next_revision: 1,
                },
            })
            .collect::<Vec<_>>();
        let mut excessive_ordinary = plan.ordinary().clone();
        excessive_ordinary.claims = excessive_bound
            .iter()
            .map(|bound| bound.claim.clone())
            .collect();
        let excessive = VersionedLiveClaimCommitPlan::new(excessive_ordinary, &excessive_bound)
            .expect("oversized input remains structurally bindable before the bounded DB helper");
        assert!(
            verify_versioned_reserved_live_claims_with_conn(&conn, &scope, &excessive)
                .await
                .is_err(),
            "an oversized plan must reject before an unbounded claim-row scan"
        );
        let mutations = [
            "coverage_digest = 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'",
            "completeness = 'incomplete'",
            "owner = 'other-owner'",
            "fence_token = fence_token + 1",
            "revision = revision + 1",
            "state = 'reserved_import'",
            "logical_turn_key = 'different-key'",
            "coverage_schema_version = 3",
        ];
        for mutation in mutations {
            let txn = conn.begin().await.expect("begin isolated tuple mutation");
            let sql = format!(
                "UPDATE agent_coverage_claim SET {mutation}
                 WHERE session_id = 'claude_code__s1'
                   AND logical_turn_key = 'cov2-verify-shared'
                   AND coverage_schema_version = 2"
            );
            let changed = txn
                .execute_unprepared(&sql)
                .await
                .expect("mutate exactly one versioned claim");
            assert_eq!(changed.rows_affected(), 1, "{mutation}");
            assert!(
                verify_versioned_reserved_live_claims_with_conn(&txn, &scope, &plan)
                    .await
                    .is_err(),
                "{mutation} must reject the altered stored tuple"
            );
            txn.rollback().await.expect("restore fixture tuple");
            assert_eq!(cov2_verify_claim_rows(&conn).await, original);
        }
    }

    #[tokio::test]
    async fn cov2_versioned_reserved_live_claims_verify_same_key_and_expired_lease() {
        let (conn, scope, plan) = cov2_versioned_verify_fixture().await;
        conn.execute_unprepared(
            "UPDATE agent_coverage_claim SET lease_expires_at = 0
             WHERE session_id = 'claude_code__s1'
               AND logical_turn_key = 'cov2-verify-shared'",
        )
        .await
        .expect("expire both leases without transferring ownership");
        let expired = cov2_verify_claim_rows(&conn).await;
        verify_versioned_reserved_live_claims_with_conn(&conn, &scope, &plan)
            .await
            .expect("an expired lease does not revoke unchanged owner and fence");
        let v1 = cov2_verify_subset_plan(&plan, 0);
        let v2 = cov2_verify_subset_plan(&plan, 1);
        for (version, accepted, rejected) in [(1_i64, &v2, &v1), (2_i64, &v1, &v2)] {
            let txn = conn
                .begin()
                .await
                .expect("begin independent version mutation");
            let changed = txn
                .execute_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_coverage_claim SET owner = 'other-owner'
                     WHERE session_id = 'claude_code__s1'
                       AND logical_turn_key = 'cov2-verify-shared'
                       AND coverage_schema_version = ?",
                    [version.into()],
                ))
                .await
                .expect("change one version's owner");
            assert_eq!(changed.rows_affected(), 1);
            verify_versioned_reserved_live_claims_with_conn(&txn, &scope, accepted)
                .await
                .expect("the other version remains independently owned");
            assert!(
                verify_versioned_reserved_live_claims_with_conn(&txn, &scope, rejected)
                    .await
                    .is_err(),
                "the altered version must reject even with the same logical key"
            );
            txn.rollback().await.expect("restore independent version");
        }
        assert_eq!(cov2_verify_claim_rows(&conn).await, expired);
    }

    #[tokio::test]
    async fn cov2_versioned_reserved_live_claims_verify_is_read_only_in_caller_transaction() {
        let (conn, scope, plan) = cov2_versioned_verify_fixture().await;
        let committed = cov2_verify_claim_rows(&conn).await;
        let mut changed_bound = plan
            .claims()
            .map(
                |(coverage_schema_version, claim)| VersionedReservedTurnClaim {
                    coverage_schema_version,
                    claim: claim.clone(),
                },
            )
            .collect::<Vec<_>>();
        changed_bound[1].claim.coverage_digest = "e".repeat(64);
        let mut changed_ordinary = plan.ordinary().clone();
        changed_ordinary.claims = changed_bound
            .iter()
            .map(|bound| bound.claim.clone())
            .collect();
        let changed_plan = VersionedLiveClaimCommitPlan::new(changed_ordinary, &changed_bound)
            .expect("bind deliberate uncommitted digest mutation");
        let txn = conn
            .begin()
            .await
            .expect("begin caller-owned pending transaction");
        let changed = txn
            .execute_unprepared(
                "UPDATE agent_coverage_claim
                 SET coverage_digest = 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'
                 WHERE session_id = 'claude_code__s1'
                   AND logical_turn_key = 'cov2-verify-shared'
                   AND coverage_schema_version = 2",
            )
            .await
            .expect("make only an uncommitted versioned claim change");
        assert_eq!(changed.rows_affected(), 1);
        let before_verify = cov2_verify_claim_rows(&txn).await;
        assert!(
            verify_versioned_reserved_live_claims_with_conn(&txn, &scope, &plan)
                .await
                .is_err(),
            "the old sealed tuple must reject the uncommitted DB value"
        );
        verify_versioned_reserved_live_claims_with_conn(&txn, &scope, &changed_plan)
            .await
            .expect("the caller transaction must see its own uncommitted tuple");
        assert_eq!(
            cov2_verify_claim_rows(&txn).await,
            before_verify,
            "successful and rejected verification must not write any claim column"
        );
        txn.rollback().await.expect("restore committed claims");
        assert_eq!(cov2_verify_claim_rows(&conn).await, committed);
        verify_versioned_reserved_live_claims_with_conn(&conn, &scope, &plan)
            .await
            .expect("the original plan still verifies after rollback");
        assert!(
            verify_versioned_reserved_live_claims_with_conn(&conn, &scope, &changed_plan)
                .await
                .is_err(),
            "an uncommitted mutation must not leak to the main connection"
        );
    }

    #[tokio::test]
    async fn cov2_versioned_reserved_live_claims_retain_both_versions() {
        let (conn, scope, plan) = cov2_versioned_verify_fixture().await;
        let control = reserve_turn_claims_by_version_with_capture_scope(
            &conn,
            &scope,
            "claude_code__s1",
            &[turn(
                "cov2-retain-control",
                "control",
                Completeness::Complete,
            )],
            &[1],
            "verify-owner",
            1,
            "live",
        )
        .await
        .expect("reserve an unrelated control key");
        assert_eq!(control.reserved.len(), 1);
        let control_before = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT lease_expires_at FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__s1'
                   AND logical_turn_key = 'cov2-retain-control'
                   AND coverage_schema_version = 1"
                    .to_owned(),
            ))
            .await
            .expect("read unrelated control row")
            .expect("unrelated control row exists")
            .try_get_by::<Option<i64>, _>("lease_expires_at")
            .expect("read unrelated control lease");
        let original = cov2_verify_claim_rows(&conn).await;
        conn.execute_unprepared(
            "UPDATE agent_coverage_claim SET lease_expires_at = 0
             WHERE session_id = 'claude_code__s1'
               AND logical_turn_key = 'cov2-verify-shared'",
        )
        .await
        .expect("expire both leases without changing the owner");
        let expired = cov2_verify_claim_rows(&conn).await;
        assert_ne!(expired, original);
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin caller-owned artifact transaction");
        verify_versioned_reserved_live_claims_with_conn(&txn, &scope, &plan)
            .await
            .expect("verify both expired but still-owned versioned rows");
        retain_versioned_reserved_live_claims_for_artifact_with_conn(&txn, &scope, &plan)
            .await
            .expect("retain both versions in the caller transaction");
        let retained = cov2_verify_claim_rows(&txn).await;
        assert_ne!(retained, expired);
        for version in [1_i64, 2_i64] {
            let row = txn
                .query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT lease_expires_at FROM agent_coverage_claim
                     WHERE session_id = 'claude_code__s1'
                       AND logical_turn_key = 'cov2-verify-shared'
                       AND coverage_schema_version = ?",
                    [version.into()],
                ))
                .await
                .expect("read retained version")
                .expect("versioned row remains present");
            assert_eq!(
                row.try_get_by::<Option<i64>, _>("lease_expires_at")
                    .expect("read retained lease"),
                Some(ARTIFACT_RETAINED_LEASE_MS)
            );
        }
        txn.commit().await.expect("commit both versioned leases");
        assert_eq!(cov2_verify_claim_rows(&conn).await, retained);
        let control_after = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT lease_expires_at FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__s1'
                   AND logical_turn_key = 'cov2-retain-control'
                   AND coverage_schema_version = 1"
                    .to_owned(),
            ))
            .await
            .expect("read unrelated control row after retain")
            .expect("unrelated control row still exists")
            .try_get_by::<Option<i64>, _>("lease_expires_at")
            .expect("read unrelated control lease after retain");
        assert_eq!(control_after, control_before);
    }

    #[tokio::test]
    async fn cov2_versioned_reserved_live_claims_retain_stale_tuple_rejects() {
        let (conn, scope, plan) = cov2_versioned_verify_fixture().await;
        let original = cov2_verify_claim_rows(&conn).await;
        let mutations = [
            (
                1_i64,
                "coverage_digest = 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'",
            ),
            (
                2,
                "coverage_digest = 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'",
            ),
            (2, "completeness = 'incomplete'"),
            (2, "owner = 'other-owner'"),
            (2, "fence_token = fence_token + 1"),
            (2, "revision = revision + 1"),
            (2, "state = 'reserved_import'"),
            (2, "logical_turn_key = 'different-key'"),
            (2, "coverage_schema_version = 3"),
        ];
        for (version, mutation) in mutations {
            let txn = crate::internal::db::begin_write_transaction(&conn)
                .await
                .expect("begin isolated stale-tuple transaction");
            verify_versioned_reserved_live_claims_with_conn(&txn, &scope, &plan)
                .await
                .expect("verify original sealed tuple before deliberate drift");
            let changed = txn
                .execute_unprepared(&format!(
                    "UPDATE agent_coverage_claim SET {mutation}
                     WHERE session_id = 'claude_code__s1'
                       AND logical_turn_key = 'cov2-verify-shared'
                       AND coverage_schema_version = {version}"
                ))
                .await
                .expect("mutate exactly one versioned row");
            assert_eq!(changed.rows_affected(), 1, "{mutation}");
            let error =
                retain_versioned_reserved_live_claims_for_artifact_with_conn(&txn, &scope, &plan)
                    .await
                    .expect_err("stale sealed tuple cannot be retained");
            assert!(
                format!("{error:#}").contains("coverage ownership changed"),
                "{mutation}: {error:#}"
            );
            txn.rollback()
                .await
                .expect("discard all earlier lease writes with the stale row");
            assert_eq!(cov2_verify_claim_rows(&conn).await, original, "{mutation}");
        }
    }

    #[tokio::test]
    async fn cov2_versioned_reserved_live_claims_second_row_fault_rolls_back() {
        let (conn, scope, plan) = cov2_versioned_verify_fixture().await;
        let original = cov2_verify_claim_rows(&conn).await;
        conn.execute_unprepared(
            "CREATE TRIGGER cov2_retain_reject_second
             BEFORE UPDATE OF lease_expires_at ON agent_coverage_claim
             WHEN OLD.session_id = 'claude_code__s1'
               AND OLD.logical_turn_key = 'cov2-verify-shared'
               AND OLD.coverage_schema_version = 2
               AND EXISTS (
                   SELECT 1 FROM agent_coverage_claim
                   WHERE session_id = OLD.session_id
                     AND logical_turn_key = OLD.logical_turn_key
                     AND coverage_schema_version = 1
                     AND lease_expires_at = 9223372036854775807
               )
             BEGIN SELECT RAISE(ABORT, 'injected second-version retain failure'); END",
        )
        .await
        .expect("install real SQL fault armed only after v1 retain");
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin caller-owned artifact transaction");
        verify_versioned_reserved_live_claims_with_conn(&txn, &scope, &plan)
            .await
            .expect("verify both original sealed reservations");
        let error =
            retain_versioned_reserved_live_claims_for_artifact_with_conn(&txn, &scope, &plan)
                .await
                .expect_err("second version UPDATE must fail through the real SQLite trigger");
        assert!(format!("{error:#}").contains("injected second-version retain failure"));
        assert_ne!(
            cov2_verify_claim_rows(&txn).await,
            original,
            "the first lease was updated before the second SQL fault"
        );
        txn.rollback()
            .await
            .expect("rollback the first version after second-version SQL fault");
        assert_eq!(cov2_verify_claim_rows(&conn).await, original);
    }

    #[tokio::test]
    async fn cov2_versioned_reserved_live_claims_later_sql_fault_rolls_back() {
        let (conn, scope, plan) = cov2_versioned_verify_fixture().await;
        let original = cov2_verify_claim_rows(&conn).await;
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin caller-owned artifact transaction");
        verify_versioned_reserved_live_claims_with_conn(&txn, &scope, &plan)
            .await
            .expect("verify both original sealed reservations");
        retain_versioned_reserved_live_claims_for_artifact_with_conn(&txn, &scope, &plan)
            .await
            .expect("retain both leases before later caller SQL");
        assert_ne!(cov2_verify_claim_rows(&txn).await, original);
        let error = txn
            .execute_unprepared(
                "INSERT INTO agent_coverage_claim SELECT * FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__s1'
                   AND logical_turn_key = 'cov2-verify-shared'
                   AND coverage_schema_version = 1 LIMIT 1",
            )
            .await
            .expect_err("later SQL must hit the existing version-keyed unique key");
        assert!(format!("{error:#}").contains("UNIQUE"), "{error:#}");
        txn.rollback()
            .await
            .expect("rollback both retained leases after later caller SQL failure");
        assert_eq!(cov2_verify_claim_rows(&conn).await, original);
    }

    #[tokio::test]
    async fn cov2_claim_arbitration_by_version() {
        let conn = gate_db().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("capture scope");
        let key = "cov2-shared-key";
        let v1 = turn(key, "same ordinary prompt", Completeness::Complete);
        let v2_incomplete = cov2_visible_turn(key, "visible reasoning", Completeness::Incomplete);
        let first = reserve_turn_claims_by_version_with_capture_scope(
            &conn,
            &scope,
            "claude_code__s1",
            std::slice::from_ref(&v1),
            &[1],
            "v1-owner",
            1,
            "live",
        )
        .await
        .expect("reserve v1 claim");
        assert_eq!(first.reserved.len(), 1);
        assert_eq!(first.reserved[0].coverage_schema_version.as_i64(), 1);
        cov2_mark_claim_committed(&conn, &first.reserved[0], "v1-owner", "v1-checkpoint").await;

        let second = reserve_turn_claims_by_version_with_capture_scope(
            &conn,
            &scope,
            "claude_code__s1",
            std::slice::from_ref(&v2_incomplete),
            &[2],
            "v2-owner",
            2,
            "live",
        )
        .await
        .expect("reserve independent v2 claim");
        assert_eq!(second.reserved.len(), 1);
        assert_eq!(second.reserved[0].coverage_schema_version.as_i64(), 2);
        cov2_mark_claim_committed(&conn, &second.reserved[0], "v2-owner", "v2-checkpoint").await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'cov2-shared-key'",
            )
            .await,
            2,
            "one logical key must coexist once per version"
        );

        for (turn, version) in [(v1.clone(), 1), (v2_incomplete.clone(), 2)] {
            let duplicate = reserve_turn_claims_by_version_with_capture_scope(
                &conn,
                &scope,
                "claude_code__s1",
                &[turn],
                &[version],
                "duplicate-owner",
                3,
                "live",
            )
            .await
            .expect("equivalent same-version claim is idempotent");
            assert!(duplicate.reserved.is_empty());
            assert_eq!(duplicate.skipped_covered, 1);
        }

        let v2_complete = cov2_visible_turn(key, "visible reasoning", Completeness::Complete);
        let upgrade = reserve_turn_claims_by_version_with_capture_scope(
            &conn,
            &scope,
            "claude_code__s1",
            std::slice::from_ref(&v2_complete),
            &[2],
            "upgrade-owner",
            4,
            "live",
        )
        .await
        .expect("incomplete-to-complete v2 upgrade");
        assert_eq!(upgrade.reserved.len(), 1);
        assert_eq!(upgrade.reserved[0].claim.next_revision, 2);
        cov2_mark_claim_committed(&conn, &upgrade.reserved[0], "upgrade-owner", "v2-upgrade").await;

        let changed = cov2_visible_turn(key, "different visible reasoning", Completeness::Complete);
        let conflict = reserve_turn_claims_by_version_with_capture_scope(
            &conn,
            &scope,
            "claude_code__s1",
            &[changed],
            &[2],
            "conflict-owner",
            5,
            "live",
        )
        .await
        .expect("conflicting complete content is diagnosed");
        assert!(conflict.reserved.is_empty());
        assert_eq!(conflict.conflicted, 1);
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT coverage_schema_version, state, revision, coverage_digest FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ? ORDER BY coverage_schema_version",
                ["claude_code__s1".into(), key.into()],
            ))
            .await
            .expect("read both versions");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0]
                .try_get_by::<i64, _>("coverage_schema_version")
                .unwrap(),
            1
        );
        assert_eq!(
            rows[0].try_get_by::<String, _>("state").unwrap(),
            "catalog_committed"
        );
        assert_eq!(rows[0].try_get_by::<i64, _>("revision").unwrap(), 1);
        assert_eq!(
            rows[0].try_get_by::<String, _>("coverage_digest").unwrap(),
            first.reserved[0].claim.coverage_digest
        );
        assert_eq!(
            rows[1]
                .try_get_by::<i64, _>("coverage_schema_version")
                .unwrap(),
            2
        );
        assert_eq!(
            rows[1].try_get_by::<String, _>("state").unwrap(),
            "conflicted"
        );
        assert_eq!(rows[1].try_get_by::<i64, _>("revision").unwrap(), 2);
        assert_eq!(
            rows[1].try_get_by::<String, _>("coverage_digest").unwrap(),
            upgrade.reserved[0].claim.coverage_digest,
            "a complete conflict cannot replace the committed digest"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_conflict WHERE logical_turn_key = 'cov2-shared-key' AND coverage_schema_version = 2",
            )
            .await,
            1,
            "the conflict must have one durable version-specific diagnostic"
        );
    }

    #[tokio::test]
    async fn cov2_invalid_version_or_db_error_zero_success() {
        let conn = gate_db().await;
        let scope = CaptureScope::main_for_connection(&conn)
            .await
            .expect("capture scope");
        let ordinary = turn("invalid-version", "ordinary", Completeness::Complete);
        assert!(
            reserve_turn_claims_by_version_with_capture_scope(
                &conn,
                &scope,
                "claude_code__s1",
                &[ordinary],
                &[3],
                "owner",
                1,
                "live",
            )
            .await
            .is_err()
        );
        let typed = cov2_visible_turn("typed-v1", "visible", Completeness::Complete);
        assert!(
            reserve_turn_claims_by_version_with_capture_scope(
                &conn,
                &scope,
                "claude_code__s1",
                std::slice::from_ref(&typed),
                &[1],
                "owner",
                1,
                "live",
            )
            .await
            .is_err(),
            "typed Reasoning must not become a v1 production claim"
        );
        assert!(
            reserve_live_turn_claims_with_capture_scope(
                &conn,
                &scope,
                "claude_code__s1",
                &[typed],
                "v1-owner",
                1,
            )
            .await
            .is_err(),
            "the legacy production v1 entry point also rejects typed Reasoning"
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
            0
        );

        conn.execute_unprepared(
            "CREATE TRIGGER cov2_reject_second_claim BEFORE INSERT ON agent_coverage_claim
             WHEN NEW.logical_turn_key = 'second'
             BEGIN SELECT RAISE(ABORT, 'injected claim failure'); END",
        )
        .await
        .expect("install deterministic claim fault");
        let turns = [
            turn("first", "ordinary", Completeness::Complete),
            turn("second", "ordinary", Completeness::Complete),
        ];
        assert!(
            reserve_turn_claims_by_version_with_capture_scope(
                &conn,
                &scope,
                "claude_code__s1",
                &turns,
                &[1, 1],
                "owner",
                1,
                "live",
            )
            .await
            .is_err()
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
            0,
            "a mid-batch DB failure must roll back the first reservation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_owner_and_companion_fault_atomicity() {
        cov2_real_ref_cas_failure_and_replay().await;
    }

    #[cfg(unix)]
    async fn cov2_real_ref_cas_failure_and_replay() {
        use crate::internal::ai::{
            capture::{
                checkpoint::{
                    CheckpointConflictReason, CheckpointRedactedPayload, CheckpointStore,
                    CheckpointWriteOutcome, CheckpointWriteRequest, TracesCheckpointStore,
                    test_support,
                },
                snapshot::carrier_test_snapshot,
            },
            observed_agents::{RedactedBytes, reasoning::carrier_test_support as native},
            traces::{CheckpointScope, TracesCoverageFence},
        };

        let fixture = ReasoningFixture::new().await;
        let mut v2 = turn(
            "shared-turn",
            "same ordinary prompt",
            Completeness::Complete,
        );
        let (_, visible) =
            crate::internal::ai::observed_agents::reasoning::classify_provider_visible_reasoning(
                crate::internal::ai::observed_agents::reasoning::ReasoningProvider::ClaudeCode,
                crate::internal::ai::observed_agents::reasoning::ReasoningSourceKind::Reasoning,
                "visible reasoning".to_owned(),
            );
        v2.records.push(SemanticRecord::Reasoning {
            provider:
                crate::internal::ai::observed_agents::reasoning::ReasoningProvider::ClaudeCode,
            source_kind: Some(
                crate::internal::ai::observed_agents::reasoning::ReasoningSourceKind::Reasoning,
            ),
            text: visible,
        });
        let turns = [
            turn(
                "shared-turn",
                "same ordinary prompt",
                Completeness::Complete,
            ),
            v2,
        ];
        let reserved = reserve_turn_claims_by_version_with_capture_scope(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            &turns,
            &[1, 2],
            "cov2-store-owner",
            Utc::now().timestamp_millis(),
            "live",
        )
        .await
        .expect("reserve same key in two coverage domains");
        assert_eq!(reserved.reserved.len(), 2);
        let fences = reserved
            .reserved
            .iter()
            .map(|bound| (bound.coverage_schema_version, &bound.claim))
            .map(|(version, claim)| TracesCoverageFence {
                coverage_schema_version: version,
                logical_turn_key: &claim.logical_turn_key,
                coverage_digest: &claim.coverage_digest,
                completeness: claim.completeness,
                next_revision: claim.next_revision,
                owner: "cov2-store-owner",
                fence_token: claim.fence_token,
                reservation_state: "reserved_live",
            })
            .collect::<Vec<_>>();
        let sidecar = |bytes: &[u8]| RedactedBytes::new_unchecked(bytes.to_vec());
        let payload = CheckpointRedactedPayload::from_snapshot(
            carrier_test_snapshot(&native::NONE, b"ordinary".to_vec()),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("build ordinary payload for visible typed turn");
        let object_index_before =
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM object_index").await;
        fixture
            .conn
            .execute_unprepared(
                "CREATE TRIGGER cov2_real_reject_v2_revision BEFORE INSERT ON agent_coverage_revision
                 WHEN NEW.coverage_schema_version = 2
                 BEGIN SELECT RAISE(ABORT, 'injected v2 revision failure'); END",
            )
            .await
            .expect("inject actual companion fault");
        let retry_checkpoint_id = "cov2-real-success";
        for (fault, checkpoint_id) in [
            ("revision", retry_checkpoint_id),
            ("owner", "cov2-owner-loss"),
            ("none", retry_checkpoint_id),
        ] {
            if fault == "owner" {
                fixture
                    .conn
                    .execute_unprepared("DROP TRIGGER cov2_real_reject_v2_revision")
                    .await
                    .expect("remove actual companion fault before owner-loss trial");
            }
            let plan = VersionedLiveClaimCommitPlan::new(
                LiveClaimCommitPlan {
                    source_channel: "live",
                    session_id: native::CAPTURE.to_owned(),
                    checkpoint_id: checkpoint_id.to_owned(),
                    owner: "cov2-store-owner".to_owned(),
                    parent_commit: None,
                    created_at: 1,
                    now_ms: Utc::now().timestamp_millis(),
                    claims: reserved
                        .reserved
                        .iter()
                        .map(|bound| bound.claim.clone())
                        .collect(),
                    import_session: None,
                    import_identity: None,
                    capture_scope: Some(fixture.scope.clone()),
                },
                &reserved.reserved,
            )
            .expect("bind exact sidecar");
            let store = TracesCheckpointStore::new(
                &fixture.conn,
                fixture.dir.path(),
                native::CAPTURE,
                checkpoint_id,
                &fences,
            )
            .expect("prepare real store")
            .with_capture_scope(fixture.scope.clone());
            let request = CheckpointWriteRequest::new(
                checkpoint_id,
                checkpoint_id,
                native::CAPTURE,
                "claude_code",
                None,
                CheckpointScope::Committed,
                store.marker_generation(),
                None,
                &payload,
                None,
                None,
            )
            .expect("build request")
            .with_versioned_claims(&plan)
            .expect("bind exact versioned plan");
            let result = if fault == "owner" {
                let pause = test_support::pause_after_registration_once(fixture.dir.path());
                let preempt = async {
                    pause.wait_until_entered().await;
                    let preempted = fixture
                        .conn
                        .execute_raw(Statement::from_sql_and_values(
                            fixture.conn.get_database_backend(),
                            "UPDATE agent_coverage_claim SET owner = 'preempting-owner'
                             WHERE session_id = ? AND logical_turn_key = 'shared-turn'
                               AND coverage_schema_version = 2 AND state = 'reserved_live'",
                            [native::CAPTURE.into()],
                        ))
                        .await
                        .expect("preempt v2 owner after marker registration");
                    assert_eq!(preempted.rows_affected(), 1);
                    pause.release();
                };
                let (write, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    tokio::join!(store.write(request), preempt)
                })
                .await
                .expect("paused owner-loss writer must finish");
                write
            } else {
                store.write(request).await
            };
            if fault != "none" {
                assert!(
                    result.is_err(),
                    "real v2 {fault} fault must reject: {result:?}"
                );
                assert_eq!(
                    scalar(
                        &fixture.conn,
                        "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
                    )
                    .await,
                    0,
                    "real traces ref must roll back"
                );
                assert_eq!(
                    scalar(&fixture.conn, &format!("SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = '{checkpoint_id}'")).await,
                    0,
                    "catalog must roll back"
                );
                assert_eq!(
                    scalar(&fixture.conn, &format!("SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'")).await,
                    0,
                    "both revision rows must roll back"
                );
                assert_eq!(
                    scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'shared-turn' AND state = 'reserved_live' AND revision = 0").await,
                    2,
                    "both reserved claims must remain"
                );
                assert_eq!(
                    scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM object_index").await,
                    object_index_before,
                    "failed ref transaction must not publish object index rows"
                );
                if fault == "owner" {
                    assert_eq!(
                        scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'shared-turn' AND coverage_schema_version = 2 AND owner = 'preempting-owner'").await,
                        1,
                        "failed ref CAS must not retake the lost owner"
                    );
                    fixture
                        .conn
                        .execute_unprepared(
                            "UPDATE agent_coverage_claim SET owner = 'cov2-store-owner'
                             WHERE logical_turn_key = 'shared-turn' AND coverage_schema_version = 2
                               AND owner = 'preempting-owner' AND state = 'reserved_live'",
                        )
                        .await
                        .expect("restore original owner for clean retry fixture");
                }
                // The failed append can leave a cleanup-pending marker owning
                // newly written but unpublished objects. Retire that exact
                // failed attempt through the production repair path before
                // retrying the same checkpoint identity with a fresh store.
                let marker = crate::internal::metadata::MetadataKv::get_with_conn(
                    &fixture.conn,
                    crate::internal::metadata::MetadataScope::AgentTracesInflight,
                    native::CAPTURE,
                    checkpoint_id,
                )
                .await
                .expect("read failed checkpoint marker");
                if let Some(marker) = marker {
                    let marker =
                        crate::internal::ai::traces::decode_and_validate_traces_inflight_marker(
                            &marker.value,
                            native::CAPTURE,
                            checkpoint_id,
                        )
                        .expect("failed attempt marker must remain valid");
                    assert!(
                        marker.cleanup_pending,
                        "failed append with unpublished objects must keep recoverable ownership"
                    );
                    let storage = std::sync::Arc::new(
                        crate::utils::client_storage::ClientStorage::try_init(
                            fixture.dir.path().join("objects"),
                        )
                        .expect("storage"),
                    );
                    let manager = crate::internal::ai::history::HistoryManager::for_traces(
                        storage,
                        fixture.dir.path().to_path_buf(),
                        std::sync::Arc::new(fixture.conn.clone()),
                    );
                    assert!(
                        manager
                            .repair_expired_traces_inflight_marker_for_test(
                                native::CAPTURE,
                                checkpoint_id,
                                Utc::now().timestamp_millis(),
                            )
                            .await
                            .expect("repair failed checkpoint's cleanup marker"),
                        "failed checkpoint marker must be repairable before same-ID retry"
                    );
                }
                assert!(
                    crate::internal::metadata::MetadataKv::get_with_conn(
                        &fixture.conn,
                        crate::internal::metadata::MetadataScope::AgentTracesInflight,
                        native::CAPTURE,
                        checkpoint_id,
                    )
                    .await
                    .expect("read marker after failed-attempt repair")
                    .is_none(),
                    "same checkpoint ID retry requires the failed marker to be retired"
                );
            } else {
                assert!(matches!(result, Ok(CheckpointWriteOutcome::Written { .. })));
                assert_eq!(checkpoint_id, retry_checkpoint_id);
                assert_eq!(
                    scalar(&fixture.conn, &format!("SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'")).await,
                    2,
                    "the failed v2 checkpoint ID must commit both revisions on its clean retry"
                );
                assert_eq!(
                    scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'shared-turn' AND state = 'catalog_committed'").await,
                    2
                );
                let replay_store = TracesCheckpointStore::new(
                    &fixture.conn,
                    fixture.dir.path(),
                    native::CAPTURE,
                    checkpoint_id,
                    &fences,
                )
                .expect("prepare replay store")
                .with_capture_scope(fixture.scope.clone());
                let replay = CheckpointWriteRequest::new(
                    checkpoint_id,
                    checkpoint_id,
                    native::CAPTURE,
                    "claude_code",
                    None,
                    CheckpointScope::Committed,
                    replay_store.marker_generation(),
                    None,
                    &payload,
                    None,
                    None,
                )
                .expect("build exact replay")
                .with_versioned_claims(&plan)
                .expect("bind exact replay sidecar");
                assert!(matches!(
                    replay_store.write(replay).await,
                    Ok(CheckpointWriteOutcome::AlreadyExists { .. })
                ));
                let empty_store = TracesCheckpointStore::new(
                    &fixture.conn,
                    fixture.dir.path(),
                    native::CAPTURE,
                    checkpoint_id,
                    &[],
                )
                .expect("prepare metadata-only replay store")
                .with_capture_scope(fixture.scope.clone());
                let empty_replay = CheckpointWriteRequest::new(
                    checkpoint_id,
                    checkpoint_id,
                    native::CAPTURE,
                    "claude_code",
                    None,
                    CheckpointScope::Committed,
                    empty_store.marker_generation(),
                    None,
                    &payload,
                    None,
                    None,
                )
                .expect("build metadata-only replay request");
                assert!(
                    matches!(
                        empty_store.write(empty_replay).await,
                        Ok(CheckpointWriteOutcome::ConflictUnchanged {
                            reason: CheckpointConflictReason::ReplayPayloadMismatch
                        })
                    ),
                    "empty expected claims must not acknowledge a claim-bearing checkpoint"
                );
            }
        }

        // A committed checkpoint can arrive after the optimistic replay probe
        // but before the marker-registration writer lock. Registration must
        // compare the authoritative empty set against the new durable v2
        // revision and reject without creating a marker or append.
        let race = ReasoningFixture::new().await;
        let race_checkpoint_id = "cov2-registration-race";
        let race_store = TracesCheckpointStore::new(
            &race.conn,
            race.dir.path(),
            native::CAPTURE,
            race_checkpoint_id,
            &[],
        )
        .expect("prepare empty-claim race store")
        .with_capture_scope(race.scope.clone());
        let race_request = CheckpointWriteRequest::new(
            race_checkpoint_id,
            race_checkpoint_id,
            native::CAPTURE,
            "claude_code",
            None,
            CheckpointScope::Committed,
            race_store.marker_generation(),
            None,
            &payload,
            None,
            None,
        )
        .expect("build empty-claim race request");
        let race_pause = test_support::pause_before_registration_once(race.dir.path());
        let seed_committed = async {
            race_pause.wait_until_entered().await;
            assert_eq!(
                scalar(
                    &race.conn,
                    "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
                )
                .await,
                0,
                "the optimistic probe must not append the traces ref"
            );
            assert!(
                crate::internal::metadata::MetadataKv::get_with_conn(
                    &race.conn,
                    crate::internal::metadata::MetadataScope::AgentTracesInflight,
                    native::CAPTURE,
                    race_checkpoint_id,
                )
                .await
                .expect("read marker while writer is paused")
                .is_none(),
                "the registration race must begin before marker mutation"
            );
            race.conn
                .execute_raw(Statement::from_sql_and_values(
                    race.conn.get_database_backend(),
                    "INSERT INTO agent_checkpoint
                     (checkpoint_id, session_id, scope, parent_commit, tree_oid,
                      metadata_blob_oid, traces_commit, created_at)
                     VALUES (?, ?, 'committed', ?, ?, ?, ?, 1)",
                    [
                        race_checkpoint_id.into(),
                        native::CAPTURE.into(),
                        "a".repeat(40).into(),
                        "b".repeat(40).into(),
                        "c".repeat(40).into(),
                        "d".repeat(40).into(),
                    ],
                ))
                .await
                .expect("race in a same-session committed checkpoint");
            race.conn
                .execute_raw(Statement::from_sql_and_values(
                    race.conn.get_database_backend(),
                    "INSERT INTO agent_coverage_revision
                     (session_id, logical_turn_key, coverage_schema_version, revision,
                      checkpoint_id, coverage_digest, completeness, source_channel, created_at)
                     VALUES (?, 'raced-v2-turn', 2, 1, ?, ?, 'complete', 'live', 1)",
                    [
                        native::CAPTURE.into(),
                        race_checkpoint_id.into(),
                        "e".repeat(64).into(),
                    ],
                ))
                .await
                .expect("race in a nonempty v2 committed revision set");
            race_pause.release();
        };
        let (raced_write, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(race_store.write(race_request), seed_committed)
        })
        .await
        .expect("registration-race writer must finish");
        assert!(
            matches!(
                &raced_write,
                Ok(CheckpointWriteOutcome::ConflictUnchanged {
                    reason: CheckpointConflictReason::ReplayPayloadMismatch
                })
            ),
            "registration must reject the raced nonempty v2 claim set: {raced_write:?}"
        );
        assert_eq!(
            scalar(&race.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1,
            "the raced catalog row must remain the only checkpoint"
        );
        assert_eq!(
            scalar(
                &race.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision"
            )
            .await,
            1,
            "registration must not mutate the raced revision"
        );
        assert_eq!(
            scalar(&race.conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
            0,
            "registration must not create or mutate claims"
        );
        assert_eq!(
            scalar(
                &race.conn,
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
            )
            .await,
            0,
            "registration conflict must not append a traces ref"
        );
        assert_eq!(
            scalar(&race.conn, "SELECT COUNT(*) AS n FROM object_index").await,
            0,
            "registration conflict must not publish objects"
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &race.conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                native::CAPTURE,
                race_checkpoint_id,
            )
            .await
            .expect("read marker after registration conflict")
            .is_none(),
            "registration conflict must leave no marker"
        );
    }

    #[cfg(unix)]
    async fn cov2_full_claim_rows(conn: &DatabaseConnection, session_id: &str) -> Vec<String> {
        // `quote` and `hex` distinguish NULL, numeric and text values without
        // relying on JSON1 or an ambiguous delimiter inside a stored value.
        const COLUMNS: [&str; 16] = [
            "session_id",
            "logical_turn_key",
            "coverage_schema_version",
            "coverage_digest",
            "completeness",
            "revision",
            "state",
            "attempt_checkpoint_id",
            "owner",
            "lease_expires_at",
            "fence_token",
            "checkpoint_id",
            "traces_commit",
            "source_channel",
            "created_at",
            "updated_at",
        ];
        let projection = COLUMNS
            .iter()
            .map(|column| format!("hex(quote({column}))"))
            .collect::<Vec<_>>()
            .join(" || ':' || ");
        let sql = format!(
            "SELECT {projection} AS row_fingerprint FROM agent_coverage_claim \
             WHERE session_id = ? ORDER BY coverage_schema_version, logical_turn_key"
        );
        conn.query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            &sql,
            [session_id.into()],
        ))
        .await
        .expect("read full claim rows before or after fault")
        .into_iter()
        .map(|row| {
            row.try_get_by("row_fingerprint")
                .expect("read byte-exact full claim row")
        })
        .collect()
    }

    #[cfg(unix)]
    pub(crate) async fn cov2_native_v2_source_and_reservation(
        fixture: &ReasoningFixture,
        payload: &crate::internal::ai::capture::checkpoint::CheckpointRedactedPayload,
        turns: &[NormalizedTurn],
        owner: &str,
    ) -> (
        NativeVersionedReasoningCoveragePlan,
        Vec<VersionedReservedTurnClaim>,
    ) {
        use crate::internal::ai::{
            capture::checkpoint::{
                ReasoningIdentityPreparation, prepare_native_versioned_reasoning_coverage_plan,
            },
            observed_agents::reasoning::carrier_test_support as native,
        };
        let source = prepare_native_versioned_reasoning_coverage_plan(
            payload,
            ReasoningIdentityPreparation {
                conn: &fixture.conn,
                scope: &fixture.scope,
                storage_path: &fixture.dir.path().join(crate::utils::util::ROOT_DIR),
                authorized_root: fixture.dir.path(),
                session_id: native::CAPTURE,
                provider: "claude_code",
                turns,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )
        .await
        .expect("derive a sealed versioned source from the synthetic native fixture");
        let versions = turns
            .iter()
            .map(|turn| select_coverage_schema(turn).as_i64())
            .collect::<Vec<_>>();
        let reserved = reserve_turn_claims_with_reasoning_inner_versioned(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            turns,
            Some(&versions),
            owner,
            Utc::now().timestamp_millis(),
            "live",
            None,
            ReasoningCoverageInput::native_versioned(&source),
            CoverageVersionAdmission::Automatic,
        )
        .await
        .expect("reserve source-bound versioned claims")
        .into_versioned()
        .expect("retain exact version sidecar");
        assert_eq!(reserved.reserved.len(), turns.len());
        (source, reserved.reserved)
    }

    #[cfg(unix)]
    fn cov2_native_v2_companion(
        fixture: &ReasoningFixture,
        source: &NativeVersionedReasoningCoveragePlan,
        reserved: &[VersionedReservedTurnClaim],
        owner: &str,
        checkpoint_id: &str,
    ) -> anyhow::Result<NativeVersionedReasoningClaimCommitPlan> {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: native::CAPTURE.to_owned(),
            checkpoint_id: checkpoint_id.to_owned(),
            owner: owner.to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: Utc::now().timestamp_millis(),
            claims: reserved.iter().map(|bound| bound.claim.clone()).collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let versioned = VersionedLiveClaimCommitPlan::new(ordinary, reserved)?;
        NativeVersionedReasoningClaimCommitPlan::new(versioned, source)
    }

    #[cfg(unix)]
    pub(crate) async fn cov2_native_v2_write(
        fixture: &ReasoningFixture,
        payload: &crate::internal::ai::capture::checkpoint::CheckpointRedactedPayload,
        source: &NativeVersionedReasoningCoveragePlan,
        reserved: &[VersionedReservedTurnClaim],
        owner: &str,
        checkpoint_id: &str,
    ) -> anyhow::Result<crate::internal::ai::capture::checkpoint::CheckpointWriteOutcome> {
        use crate::internal::ai::{
            capture::checkpoint::{CheckpointStore, CheckpointWriteRequest, TracesCheckpointStore},
            observed_agents::reasoning::carrier_test_support as native,
            traces::{CheckpointScope, TracesCoverageFence},
        };
        let companion = cov2_native_v2_companion(fixture, source, reserved, owner, checkpoint_id)?;
        let fences = reserved
            .iter()
            .map(|bound| TracesCoverageFence {
                coverage_schema_version: bound.coverage_schema_version,
                logical_turn_key: &bound.claim.logical_turn_key,
                coverage_digest: &bound.claim.coverage_digest,
                completeness: bound.claim.completeness,
                next_revision: bound.claim.next_revision,
                owner,
                fence_token: bound.claim.fence_token,
                reservation_state: "reserved_live",
            })
            .collect::<Vec<_>>();
        let store = TracesCheckpointStore::new(
            &fixture.conn,
            fixture.dir.path(),
            native::CAPTURE,
            checkpoint_id,
            &fences,
        )?
        .with_capture_scope(fixture.scope.clone());
        let request = CheckpointWriteRequest::new(
            checkpoint_id,
            checkpoint_id,
            native::CAPTURE,
            "claude_code",
            None,
            CheckpointScope::Committed,
            store.marker_generation(),
            None,
            payload,
            None,
            None,
        )?
        .with_native_versioned_claims(&companion)?;
        Ok(store.write(request).await?)
    }

    #[cfg(unix)]
    async fn cov2_ordinary_versioned_write(
        fixture: &ReasoningFixture,
        reserved: &[VersionedReservedTurnClaim],
        owner: &str,
        checkpoint_id: &str,
    ) -> anyhow::Result<crate::internal::ai::capture::checkpoint::CheckpointWriteOutcome> {
        use crate::internal::ai::{
            capture::{
                checkpoint::{
                    CheckpointRedactedPayload, CheckpointStore, CheckpointWriteRequest,
                    TracesCheckpointStore,
                },
                snapshot::carrier_test_snapshot,
            },
            observed_agents::{RedactedBytes, reasoning::carrier_test_support as native},
            traces::{CheckpointScope, TracesCoverageFence},
        };
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: native::CAPTURE.to_owned(),
            checkpoint_id: checkpoint_id.to_owned(),
            owner: owner.to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: Utc::now().timestamp_millis(),
            claims: reserved.iter().map(|bound| bound.claim.clone()).collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let plan = VersionedLiveClaimCommitPlan::new(ordinary, reserved)?;
        let fences = reserved
            .iter()
            .map(|bound| TracesCoverageFence {
                coverage_schema_version: bound.coverage_schema_version,
                logical_turn_key: &bound.claim.logical_turn_key,
                coverage_digest: &bound.claim.coverage_digest,
                completeness: bound.claim.completeness,
                next_revision: bound.claim.next_revision,
                owner,
                fence_token: bound.claim.fence_token,
                reservation_state: "reserved_live",
            })
            .collect::<Vec<_>>();
        let sidecar = |bytes: &[u8]| RedactedBytes::new_unchecked(bytes.to_vec());
        let payload = CheckpointRedactedPayload::from_snapshot(
            carrier_test_snapshot(&native::NONE, b"ordinary checkpoint".to_vec()),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )?;
        let store = TracesCheckpointStore::new(
            &fixture.conn,
            fixture.dir.path(),
            native::CAPTURE,
            checkpoint_id,
            &fences,
        )?
        .with_capture_scope(fixture.scope.clone());
        let request = CheckpointWriteRequest::new(
            checkpoint_id,
            checkpoint_id,
            native::CAPTURE,
            "claude_code",
            None,
            CheckpointScope::Committed,
            store.marker_generation(),
            None,
            &payload,
            None,
            None,
        )?
        .with_versioned_claims(&plan)?;
        Ok(store.write(request).await?)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_native_v2_replay_requires_authenticated_identity() {
        use std::time::Duration;

        use crate::internal::ai::{
            capture::checkpoint::{
                CheckpointConflictReason, CheckpointStoreError, CheckpointStoreStage,
                CheckpointWriteOutcome, reasoning_coverage_test_payload, test_support,
            },
            observed_agents::reasoning::carrier_test_support as native,
        };

        let payload = reasoning_coverage_test_payload(vec![b"synthetic sealed replay".to_vec()]);
        let turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "typed replay turn",
            Completeness::Complete,
        )];
        let initial_turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "typed replay turn",
            Completeness::Incomplete,
        )];
        let owner = "native-replay-owner";
        let checkpoint_id = "native-replay-checkpoint";
        let native_fixture = ReasoningFixture::new().await;
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&native_fixture, &payload, &initial_turns, owner)
                .await;
        assert!(matches!(
            cov2_native_v2_write(
                &native_fixture,
                &payload,
                &source,
                &reserved,
                owner,
                checkpoint_id
            )
            .await,
            Ok(CheckpointWriteOutcome::Written { .. })
        ));
        assert_eq!(
            cov2_native_v2_write(
                &native_fixture,
                &payload,
                &source,
                &reserved,
                owner,
                checkpoint_id
            )
            .await
            .expect("exact native replay"),
            CheckpointWriteOutcome::AlreadyExists {
                checkpoint_id: checkpoint_id.to_owned(),
            },
            "an authenticated native identity must still acknowledge its exact replay"
        );

        let saved_v2_map = crate::internal::metadata::MetadataKv::get_with_conn(
            &native_fixture.conn,
            crate::internal::metadata::MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            crate::internal::metadata::ReasoningIdentitySessionMapV2::KEY,
        )
        .await
        .expect("read committed native map")
        .expect("native writer must commit its v2 map")
        .value;
        let mut tampered_map: serde_json::Value =
            serde_json::from_str(&saved_v2_map).expect("decode test-only map");
        let mac = tampered_map["mac"]
            .as_str()
            .expect("map MAC must be a string");
        let changed_mac = format!(
            "{}{}",
            &mac[..mac.len() - 1],
            if mac.ends_with('a') { 'b' } else { 'a' }
        );
        tampered_map["mac"] = changed_mac.into();
        crate::internal::metadata::MetadataKv::set_with_conn(
            &native_fixture.conn,
            crate::internal::metadata::MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            crate::internal::metadata::ReasoningIdentitySessionMapV2::KEY,
            &tampered_map.to_string(),
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("tamper only the test map MAC");
        let tampered_replay = cov2_native_v2_write(
            &native_fixture,
            &payload,
            &source,
            &reserved,
            owner,
            checkpoint_id,
        )
        .await
        .expect_err("tampered private map must not ACK native replay");
        assert!(matches!(
            tampered_replay.downcast_ref::<CheckpointStoreError>(),
            Some(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::CatalogTransaction,
            })
        ));
        crate::internal::metadata::MetadataKv::set_with_conn(
            &native_fixture.conn,
            crate::internal::metadata::MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            crate::internal::metadata::ReasoningIdentitySessionMapV2::KEY,
            &saved_v2_map,
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("restore authenticated map before same-slot supersession");

        let mut later_turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "typed replay turn at a later revision",
            Completeness::Complete,
        )];
        later_turns[0].records[0] = SemanticRecord::User {
            text: "changed ordinary prompt at a later revision".to_owned(),
        };
        let (later_source, later_reserved) =
            cov2_native_v2_source_and_reservation(&native_fixture, &payload, &later_turns, owner)
                .await;
        assert!(
            later_reserved[0].claim.next_revision > reserved[0].claim.next_revision,
            "the second checkpoint must supersede the same identity slot"
        );
        assert!(matches!(
            cov2_native_v2_write(
                &native_fixture,
                &payload,
                &later_source,
                &later_reserved,
                owner,
                "native-replay-checkpoint-next",
            )
            .await,
            Ok(CheckpointWriteOutcome::Written { .. })
        ));
        assert_eq!(
            cov2_native_v2_write(
                &native_fixture,
                &payload,
                &source,
                &reserved,
                owner,
                checkpoint_id,
            )
            .await
            .expect("older native replay after same-slot supersession"),
            CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ReplayPayloadMismatch,
            },
            "an older native replay must fail closed when the current session map no longer proves its identity"
        );

        let ordinary = ReasoningFixture::new().await;
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&ordinary, &payload, &turns, owner).await;
        assert!(matches!(
            cov2_ordinary_versioned_write(&ordinary, &reserved, owner, checkpoint_id).await,
            Ok(CheckpointWriteOutcome::Written { .. })
        ));
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &ordinary.conn,
                crate::internal::metadata::MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                crate::internal::metadata::ReasoningIdentitySessionMapV2::KEY,
            )
            .await
            .expect("read ordinary checkpoint private-map slot")
            .is_none(),
            "ordinary versioned commit must not create native identity evidence"
        );
        assert_eq!(
            cov2_native_v2_write(
                &ordinary,
                &payload,
                &source,
                &reserved,
                owner,
                checkpoint_id
            )
            .await
            .expect("native replay of ordinary versioned checkpoint"),
            CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ReplayPayloadMismatch,
            },
            "matching revisions without native identity must not falsely acknowledge a replay"
        );

        let ordinary_with_marker = ReasoningFixture::new().await;
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&ordinary_with_marker, &payload, &turns, owner)
                .await;
        test_support::fail_marker_cleanup_once();
        assert!(matches!(
            cov2_ordinary_versioned_write(&ordinary_with_marker, &reserved, owner, checkpoint_id,)
                .await,
            Ok(CheckpointWriteOutcome::PendingCleanup { .. })
        ));
        let marker_before = crate::internal::metadata::MetadataKv::get_with_conn(
            &ordinary_with_marker.conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            native::CAPTURE,
            checkpoint_id,
        )
        .await
        .expect("read retained ordinary marker")
        .expect("ordinary cleanup fault must retain marker")
        .value;
        assert_eq!(
            cov2_native_v2_write(
                &ordinary_with_marker,
                &payload,
                &source,
                &reserved,
                owner,
                checkpoint_id,
            )
            .await
            .expect("native replay of ordinary checkpoint with marker"),
            CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ReplayPayloadMismatch,
            },
        );
        let marker_after = crate::internal::metadata::MetadataKv::get_with_conn(
            &ordinary_with_marker.conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            native::CAPTURE,
            checkpoint_id,
        )
        .await
        .expect("read marker after rejected native replay")
        .expect("rejected native replay must retain marker")
        .value;
        assert_eq!(marker_after, marker_before);

        let raced = ReasoningFixture::new().await;
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&raced, &payload, &turns, owner).await;
        let pause = test_support::pause_before_registration_once(raced.dir.path());
        let native_write =
            cov2_native_v2_write(&raced, &payload, &source, &reserved, owner, checkpoint_id);
        let ordinary_winner = async {
            pause.wait_until_entered().await;
            let outcome =
                cov2_ordinary_versioned_write(&raced, &reserved, owner, checkpoint_id).await;
            pause.release();
            outcome
        };
        let (native_outcome, ordinary_outcome) =
            tokio::time::timeout(Duration::from_secs(20), async {
                tokio::join!(native_write, ordinary_winner)
            })
            .await
            .expect("registration race must complete");
        assert!(matches!(
            ordinary_outcome,
            Ok(CheckpointWriteOutcome::Written { .. })
        ));
        assert_eq!(
            native_outcome.expect("native registration-race outcome"),
            CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ReplayPayloadMismatch,
            },
            "AlreadyCommitted after the replay probe must verify native identity too"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_native_v2_claim_tuple_binding() {
        use crate::internal::ai::{
            capture::checkpoint::{
                ReasoningIdentityPreparation, prepare_native_versioned_reasoning_coverage_plan,
                reasoning_coverage_test_native_batch,
                reasoning_coverage_test_payload_for_native_turns,
            },
            observed_agents::reasoning::carrier_test_support as native,
        };
        let fixture = ReasoningFixture::new().await;
        let payload = reasoning_coverage_test_payload_for_native_turns(vec![
            (0, b"synthetic bound field 0".to_vec()),
            (1, b"synthetic bound field 1".to_vec()),
        ]);
        let turns = [
            turn("synthetic-turn-0", "ordinary", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-0", "typed visible", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-1", "typed visible", Completeness::Complete),
        ];
        let proof_keys = reasoning_coverage_test_native_batch(&payload)
            .pending_logical_turns()
            .collect::<Vec<_>>();
        assert_eq!(proof_keys, ["synthetic-turn-0", "synthetic-turn-1"]);
        assert!(
            !turns[..2]
                .iter()
                .any(|turn| turn.logical_turn_key == proof_keys[1]),
            "the orphan proof key must be absent from the proposed normalized turns"
        );
        let orphan = prepare_native_versioned_reasoning_coverage_plan(
            &payload,
            ReasoningIdentityPreparation {
                conn: &fixture.conn,
                scope: &fixture.scope,
                storage_path: &fixture.dir.path().join(crate::utils::util::ROOT_DIR),
                authorized_root: fixture.dir.path(),
                session_id: native::CAPTURE,
                provider: "claude_code",
                turns: &turns[..2],
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )
        .await;
        let orphan_error = match orphan {
            Ok(_) => panic!("source factory accepted a proof without a normalized turn"),
            Err(error) => error,
        };
        assert!(
            orphan_error
                .to_string()
                .contains("reasoning source cannot be associated with its logical turns"),
            "the factory must reject the orphan proof key before reservation: {orphan_error:#}"
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0,
            "an orphan source must not reserve claims"
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0,
            "an orphan source must not publish a checkpoint"
        );

        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&fixture, &payload, &turns, "tuple-owner").await;
        assert_eq!(reserved.len(), turns.len());
        let companion = cov2_native_v2_companion(
            &fixture,
            &source,
            &reserved,
            "tuple-owner",
            "tuple-checkpoint",
        )
        .expect("bind the complete versioned claim set to the native source");
        source
            .bind_claims(companion.versioned())
            .expect("the exact source-derived claim set remains bound");
        for key in reasoning_coverage_test_native_batch(&payload).pending_logical_turns() {
            let matching_turns = turns
                .iter()
                .filter(|turn| turn.logical_turn_key == key)
                .collect::<Vec<_>>();
            assert!(
                !matching_turns.is_empty(),
                "proof key {key} has no normalized turn"
            );
            for turn in matching_turns {
                let version = select_coverage_schema(turn);
                let identity = source.turn(version, key).unwrap_or_else(|| {
                    panic!("proof key {key} has no v{} identity", version.as_i64())
                });
                assert_eq!(
                    identity.kind(),
                    ReasoningIdentityKind::Nonempty,
                    "proof key {key} must retain its source-derived identity"
                );
                let expected = encode_coverage_turn_for_schema(version.as_i64(), turn)
                    .expect("encode the normalized proof-bearing turn");
                let claim = companion
                    .versioned()
                    .claims()
                    .find(|(bound_version, claim)| {
                        *bound_version == version && claim.logical_turn_key == key
                    })
                    .map(|(_, claim)| claim)
                    .unwrap_or_else(|| {
                        panic!("proof key {key} has no v{} bound claim", version.as_i64())
                    });
                assert_eq!(claim.coverage_digest, expected.digest_hex);
                assert_eq!(claim.completeness, turn.completeness);
            }
        }

        // The ordinary claim vector and ordinal sidecar must agree before
        // comparing their canonical version-keyed source set.
        let mut reversed = reserved.clone();
        reversed.reverse();
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: native::CAPTURE.to_owned(),
            checkpoint_id: "tuple-checkpoint".to_owned(),
            owner: "tuple-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: Utc::now().timestamp_millis(),
            claims: reserved.iter().map(|item| item.claim.clone()).collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let mut extra = reserved.clone();
        let mut added = extra[1].clone();
        added.claim.logical_turn_key = "extra-v2-key".to_owned();
        extra.push(added);
        let mut extra_ordinary = ordinary.clone();
        extra_ordinary.claims = extra.iter().map(|item| item.claim.clone()).collect();
        let extra_versioned = VersionedLiveClaimCommitPlan::new(extra_ordinary, &extra)
            .expect("the ordinal sidecar is internally consistent");
        assert!(
            source.bind_claims(&extra_versioned).is_err(),
            "an extra complete versioned claim must not bind to the sealed source"
        );
        assert!(
            cov2_native_v2_companion(&fixture, &source, &extra, "tuple-owner", "tuple-checkpoint",)
                .is_err(),
            "the production companion constructor must reject the same extra claim"
        );
        assert!(VersionedLiveClaimCommitPlan::new(ordinary, &reversed).is_err());
        assert!(
            cov2_native_v2_companion(
                &fixture,
                &source,
                &reversed,
                "tuple-owner",
                "tuple-checkpoint",
            )
            .is_ok(),
            "canonical set equality must not depend on presentation order"
        );

        for drift in ["version", "key", "digest", "completeness", "missing"] {
            let mut changed = reserved.clone();
            match drift {
                "version" => changed[1].coverage_schema_version = CoverageSchemaVersion::V1,
                "key" => changed[1].claim.logical_turn_key = "other-key".to_owned(),
                "digest" => changed[1].claim.coverage_digest = "f".repeat(64),
                "completeness" => changed[1].claim.completeness = Completeness::Incomplete,
                "missing" => {
                    changed.pop();
                }
                _ => unreachable!("closed test mutation list"),
            }
            assert!(
                cov2_native_v2_companion(
                    &fixture,
                    &source,
                    &changed,
                    "tuple-owner",
                    "tuple-checkpoint",
                )
                .is_err(),
                "{drift} must not bind a native source to different claims"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_v2_identity_domains_and_same_key() {
        use crate::internal::ai::{
            capture::checkpoint::{CheckpointWriteOutcome, reasoning_coverage_test_payload},
            observed_agents::reasoning::carrier_test_support as native,
        };
        let fixture = ReasoningFixture::new().await;
        let payload =
            reasoning_coverage_test_payload(vec![b"synthetic synthetic-turn-0 field".to_vec()]);
        let turns = [
            turn("synthetic-turn-0", "ordinary", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-0", "typed visible", Completeness::Complete),
        ];
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&fixture, &payload, &turns, "domain-owner").await;
        let outcome = cov2_native_v2_write(
            &fixture,
            &payload,
            &source,
            &reserved,
            "domain-owner",
            "domain-checkpoint",
        )
        .await
        .expect("real native-v2 store write");
        assert!(matches!(outcome, CheckpointWriteOutcome::Written { .. }));
        let reader = fixture.reader();
        let maps = read_reasoning_map(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            Some(&reader),
        )
        .await
        .expect("read authenticated private maps");
        let StoredReasoningMap::Verified(maps) = maps else {
            panic!("both production private maps must authenticate");
        };
        let slot_v1 = reader
            .turn_slot(&fixture.conn, "synthetic-turn-0")
            .await
            .unwrap();
        let slot_v2 = reader
            .turn_slot_for_schema_version(&fixture.conn, 2, "synthetic-turn-0")
            .await
            .unwrap();
        let map_v1 = maps.v1.as_ref().expect("production v1 map");
        let map_v2 = maps.v2.as_ref().expect("production v2 map");
        let record_v1 = map_v1
            .records()
            .get(&slot_v1)
            .expect("production v1 record");
        let record_v2 = map_v2
            .records()
            .get(&slot_v2)
            .expect("production v2 record");
        let source_v1 = source
            .turn(CoverageSchemaVersion::V1, "synthetic-turn-0")
            .expect("sealed v1 identity for the shared logical key");
        let source_v2 = source
            .turn(CoverageSchemaVersion::V2, "synthetic-turn-0")
            .expect("sealed v2 identity for the shared logical key");
        assert_eq!(source_v1.slot(), slot_v1);
        assert_eq!(source_v2.slot(), slot_v2);
        assert_ne!(source_v1.slot(), source_v2.slot());
        assert_eq!(source_v1.kind(), record_v1.body().kind);
        assert_eq!(source_v2.kind(), record_v2.body().kind);
        assert_eq!(source_v1.descriptor_mac(), record_v1.body().descriptor_mac);
        assert_eq!(source_v2.descriptor_mac(), record_v2.body().descriptor_mac);
        assert_ne!(slot_v1, slot_v2);
        assert_ne!(
            record_v1.mac(),
            record_v2.mac(),
            "record MAC domains must differ"
        );
        assert_ne!(map_v1.mac(), map_v2.mac(), "map MAC domains must differ");
        assert_eq!(record_v1.body().context.coverage_schema_version, 1);
        assert_eq!(record_v2.body().context.coverage_schema_version, 2);
        let v1_text = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
        )
        .await
        .unwrap()
        .unwrap()
        .value;
        let v2_text = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMapV2::KEY,
        )
        .await
        .unwrap()
        .unwrap()
        .value;
        let committed_before = scalar(
            &fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'catalog_committed'",
        )
        .await;
        let ref_before = scalar(
            &fixture.conn,
            "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'",
        )
        .await;

        let mut tampered: serde_json::Value = serde_json::from_str(&v2_text).unwrap();
        let original_mac = map_v2.mac();
        let replacement = if original_mac.ends_with('a') {
            'b'
        } else {
            'a'
        };
        let changed_mac = format!("{}{}", &original_mac[..original_mac.len() - 1], replacement);
        tampered["mac"] = serde_json::Value::String(changed_mac);
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMapV2::KEY,
            &tampered.to_string(),
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(
            matches!(
                read_reasoning_map(
                    &fixture.conn,
                    &fixture.scope,
                    native::CAPTURE,
                    Some(&reader),
                )
                .await
                .unwrap(),
                StoredReasoningMap::Invalid
            ),
            "tampered whole-map MAC must fail closed"
        );
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
            &v2_text,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMapV2::KEY,
            &v1_text,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(
            matches!(
                read_reasoning_map(
                    &fixture.conn,
                    &fixture.scope,
                    native::CAPTURE,
                    Some(&reader),
                )
                .await
                .unwrap(),
                StoredReasoningMap::Invalid
            ),
            "cross-domain map swap must fail closed"
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'catalog_committed'"
            )
            .await,
            committed_before
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
            )
            .await,
            ref_before
        );
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
            &v1_text,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMapV2::KEY,
            &v2_text,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(matches!(
            read_reasoning_map(
                &fixture.conn,
                &fixture.scope,
                native::CAPTURE,
                Some(&reader),
            )
            .await
            .unwrap(),
            StoredReasoningMap::Verified(_)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_v2_identity_aggregate_cap() {
        use crate::internal::ai::{
            capture::checkpoint::{CheckpointWriteOutcome, reasoning_coverage_test_payload},
            observed_agents::reasoning::carrier_test_support as native,
        };
        let max_bytes = crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES;
        let reference = ReasoningFixture::new().await;
        let payload = reasoning_coverage_test_payload(vec![b"synthetic bounded field".to_vec()]);
        let turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "typed visible",
            Completeness::Complete,
        )];
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&reference, &payload, &turns, "reference-owner")
                .await;
        assert!(matches!(
            cov2_native_v2_write(
                &reference,
                &payload,
                &source,
                &reserved,
                "reference-owner",
                "capacity-checkpoint",
            )
            .await
            .unwrap(),
            CheckpointWriteOutcome::Written { .. }
        ));
        let new_v2_bytes = MetadataKv::get_with_conn(
            &reference.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMapV2::KEY,
        )
        .await
        .unwrap()
        .unwrap()
        .value
        .len();
        assert!(new_v2_bytes > 0 && new_v2_bytes < max_bytes);

        // Seed only the pre-existing signed map. The crossing operation is a
        // real store write and cannot be replaced by this setup helper.
        let crossing = ReasoningFixture::new().await;
        let one = cov2_seed_bulk_signed_private_map(&crossing, 1, 1).await;
        let two = cov2_seed_bulk_signed_private_map(&crossing, 1, 2).await;
        let increment = two.len() - one.len();
        assert!(increment > 0);
        let base = one.len() - increment;
        let count = (max_bytes - new_v2_bytes - base) / increment + 1;
        assert!(count <= crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES);
        let retained_v1 = cov2_seed_bulk_signed_private_map(&crossing, 1, count).await;
        assert!(retained_v1.len() <= max_bytes);
        assert!(retained_v1.len() + new_v2_bytes > max_bytes);
        assert!(matches!(
            read_reasoning_map(
                &crossing.conn,
                &crossing.scope,
                native::CAPTURE,
                Some(&crossing.reader()),
            )
            .await
            .unwrap(),
            StoredReasoningMap::Verified(_)
        ));
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&crossing, &payload, &turns, "capacity-owner")
                .await;
        let index_before = scalar(&crossing.conn, "SELECT COUNT(*) AS n FROM object_index").await;
        let claims_before = cov2_full_claim_rows(&crossing.conn, native::CAPTURE).await;
        assert_eq!(claims_before.len(), 1);
        let result = cov2_native_v2_write(
            &crossing,
            &payload,
            &source,
            &reserved,
            "capacity-owner",
            "capacity-checkpoint",
        )
        .await;
        assert!(
            result.is_err(),
            "real v2 writer must reject a combined byte overflow"
        );
        assert_eq!(
            scalar(
                &crossing.conn,
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
            )
            .await,
            0
        );
        assert_eq!(scalar(&crossing.conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'capacity-checkpoint'").await, 0);
        assert_eq!(scalar(&crossing.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = 'capacity-checkpoint'").await, 0);
        assert_eq!(scalar(&crossing.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'synthetic-turn-0' AND state = 'reserved_live' AND revision = 0").await, 1);
        assert_eq!(
            cov2_full_claim_rows(&crossing.conn, native::CAPTURE).await,
            claims_before,
            "capacity rejection must preserve every column of the reserved claim"
        );
        assert_eq!(
            scalar(&crossing.conn, "SELECT COUNT(*) AS n FROM object_index").await,
            index_before
        );
        assert_eq!(
            MetadataKv::get_with_conn(
                &crossing.conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMap::KEY,
            )
            .await
            .unwrap()
            .unwrap()
            .value,
            retained_v1
        );
        assert!(
            MetadataKv::get_with_conn(
                &crossing.conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMapV2::KEY,
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_native_v2_history_fault_atomicity() {
        use crate::internal::ai::{
            capture::checkpoint::{
                CheckpointStoreError, CheckpointStoreStage, reasoning_coverage_test_payload,
                test_support,
            },
            observed_agents::reasoning::carrier_test_support as native,
        };
        let fixture = ReasoningFixture::new().await;
        let payload = reasoning_coverage_test_payload(vec![b"synthetic SQL fault field".to_vec()]);
        let turns = [
            turn("synthetic-turn-0", "ordinary", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-0", "typed visible", Completeness::Complete),
        ];
        let (source, reserved) =
            cov2_native_v2_source_and_reservation(&fixture, &payload, &turns, "fault-owner").await;
        let index_before = scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM object_index").await;
        let claims_before = cov2_full_claim_rows(&fixture.conn, native::CAPTURE).await;
        assert_eq!(claims_before.len(), 2);
        // The SQL fault is armed only for the marker owned by this fixture.
        // A wrong target would let the writer proceed and fail the assertion
        // below instead of silently exercising a different abort branch.
        let capture_target_sql = native::CAPTURE.replace('\'', "''");
        let trigger_sql = format!(
            "CREATE TRIGGER cov2_reject_identity_v2 BEFORE INSERT ON metadata_kv
             WHEN NEW.scope = 'agent_reasoning_identity' AND NEW.key = 'identity-v2'
               AND EXISTS (SELECT 1 FROM metadata_kv
                           WHERE scope = 'agent_traces_inflight'
                             AND target = '{capture_target_sql}'
                             AND key = 'fault-checkpoint')
             BEGIN
               SELECT RAISE(ABORT, 'injected identity-v2 SQL failure');
             END"
        );
        fixture
            .conn
            .execute_unprepared(&trigger_sql)
            .await
            .expect("inject real v2 map SQL fault with matching marker witness");
        let pause = test_support::pause_after_registration_once(fixture.dir.path());
        let observe_registered = async {
            pause.wait_until_entered().await;
            let registered = MetadataKv::get_with_conn(
                &fixture.conn,
                MetadataScope::AgentTracesInflight,
                native::CAPTURE,
                "fault-checkpoint",
            )
            .await
            .expect("read pre-registered marker before injected SQL fault")
            .expect("marker is durable before ref-CAS transaction");
            let registered =
                crate::internal::ai::traces::decode_and_validate_traces_inflight_marker(
                    &registered.value,
                    native::CAPTURE,
                    "fault-checkpoint",
                )
                .expect("pre-registered marker is valid");
            let registered_generation = registered
                .generation
                .expect("pre-registered marker has a generation");
            pause.release();
            registered_generation
        };
        let (result, registered_generation) =
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(
                    cov2_native_v2_write(
                        &fixture,
                        &payload,
                        &source,
                        &reserved,
                        "fault-owner",
                        "fault-checkpoint",
                    ),
                    observe_registered,
                )
            })
            .await
            .expect("marker-witnessed SQL-fault writer must finish");
        assert!(
            matches!(
                result
                    .as_ref()
                    .err()
                    .and_then(|error| error.downcast_ref::<CheckpointStoreError>()),
                Some(CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::CatalogTransaction
                })
            ),
            "identity-v2 SQL fault must reach the ref-CAS companion and produce no successful checkpoint ACK"
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
            )
            .await,
            0
        );
        assert_eq!(scalar(&fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'fault-checkpoint'").await, 0);
        assert_eq!(scalar(&fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = 'fault-checkpoint'").await, 0);
        assert_eq!(scalar(&fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'synthetic-turn-0' AND state = 'reserved_live' AND revision = 0").await, 2);
        assert_eq!(
            cov2_full_claim_rows(&fixture.conn, native::CAPTURE).await,
            claims_before,
            "SQL rollback must preserve every column of both versioned claims"
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_reasoning_identity'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM object_index").await,
            index_before
        );
        let failed_marker = MetadataKv::get_with_conn(
            &fixture.conn,
            MetadataScope::AgentTracesInflight,
            native::CAPTURE,
            "fault-checkpoint",
        )
        .await
        .expect("read marker after ref-CAS rollback")
        .expect("failed ref-CAS must retain its pre-registered recovery marker");
        let failed_marker =
            crate::internal::ai::traces::decode_and_validate_traces_inflight_marker(
                &failed_marker.value,
                native::CAPTURE,
                "fault-checkpoint",
            )
            .expect("failed marker remains valid for recovery");
        assert_eq!(
            failed_marker.generation.as_deref(),
            Some(registered_generation.as_str()),
            "ref-CAS rollback must not replace the registered marker generation"
        );
        assert!(
            failed_marker.cleanup_pending && !failed_marker.created_oids.is_empty(),
            "unpublished objects must remain owned by the recovery marker"
        );
        assert!(failed_marker.commit.is_none());
        fixture
            .conn
            .execute_unprepared("DROP TRIGGER cov2_reject_identity_v2")
            .await
            .expect("drop injected fault before retry");
        assert!(matches!(
            read_reasoning_map(
                &fixture.conn,
                &fixture.scope,
                native::CAPTURE,
                Some(&fixture.reader()),
            )
            .await
            .unwrap(),
            StoredReasoningMap::LegacyAbsent
        ));
        let storage = std::sync::Arc::new(
            crate::utils::client_storage::ClientStorage::try_init(
                fixture.dir.path().join("objects"),
            )
            .expect("storage"),
        );
        let manager = crate::internal::ai::history::HistoryManager::for_traces(
            storage,
            fixture.dir.path().to_path_buf(),
            std::sync::Arc::new(fixture.conn.clone()),
        );
        assert!(
            manager
                .repair_expired_traces_inflight_marker_for_test(
                    native::CAPTURE,
                    "fault-checkpoint",
                    Utc::now().timestamp_millis(),
                )
                .await
                .expect("repair the retained failed marker"),
            "the failed marker must be handled by the production repair path"
        );
        drop(manager);
        assert!(
            MetadataKv::get_with_conn(
                &fixture.conn,
                MetadataScope::AgentTracesInflight,
                native::CAPTURE,
                "fault-checkpoint",
            )
            .await
            .expect("read marker after repair")
            .is_none(),
            "repair must retire the failed marker before same-ID retry"
        );
        let retry = cov2_native_v2_write(
            &fixture,
            &payload,
            &source,
            &reserved,
            "fault-owner",
            "fault-checkpoint",
        )
        .await
        .expect("retry the real store with the same checkpoint ID");
        assert!(matches!(
            retry,
            crate::internal::ai::capture::checkpoint::CheckpointWriteOutcome::Written { .. }
        ));
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
            )
            .await,
            1
        );
        assert_eq!(scalar(&fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'fault-checkpoint'").await, 1);
        assert_eq!(scalar(&fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = 'fault-checkpoint'").await, 2);
        assert_eq!(scalar(&fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'synthetic-turn-0' AND state = 'catalog_committed'").await, 2);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_reasoning_identity'"
            )
            .await,
            2
        );
        assert!(matches!(
            read_reasoning_map(
                &fixture.conn,
                &fixture.scope,
                native::CAPTURE,
                Some(&fixture.reader()),
            )
            .await
            .expect("read private maps after same-ID retry"),
            StoredReasoningMap::Verified(_)
        ));
    }

    #[cfg(unix)]
    pub(crate) struct ReasoningFixture {
        pub(crate) dir: tempfile::TempDir,
        pub(crate) conn: DatabaseConnection,
        pub(crate) scope: CaptureScope,
    }

    #[cfg(unix)]
    impl ReasoningFixture {
        pub(crate) async fn new() -> Self {
            use crate::{
                internal::ai::observed_agents::reasoning::carrier_test_support as native,
                utils::util,
            };
            let dir = tempfile::tempdir().unwrap();
            let storage = dir.path().join(util::ROOT_DIR);
            std::fs::create_dir(&storage).unwrap();
            let conn = crate::internal::db::create_database(
                storage.join(util::DATABASE).to_str().unwrap(),
            )
            .await
            .unwrap();
            crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
                .await
                .unwrap();
            let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
            conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir,
                 metadata_json, started_at, last_event_at, sync_revision, repo_id, worktree_id, scope_state)
                 VALUES (?, 'claude_code', ?, 'active', ?, '{}', 1, 1, 1, ?, '', 'scoped')",
                [native::CAPTURE.into(), native::SOURCE.into(), dir.path().to_string_lossy().into_owned().into(), scope.repo_id.clone().into()]
            )).await.unwrap();
            crate::internal::ai::capture::key::load_capture_dedup_secret_with_mutation_deadline(
                &storage, None,
            )
            .unwrap();
            Self { dir, conn, scope }
        }

        pub(crate) async fn seed_legacy_v1_checkpoint_revision(&self, checkpoint_id: &str) {
            use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
            self.conn
                .execute_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "INSERT INTO agent_checkpoint
                     (checkpoint_id, session_id, scope, parent_commit, tree_oid,
                      metadata_blob_oid, traces_commit, created_at)
                     VALUES (?, ?, 'committed', ?, ?, ?, ?, 1)",
                    [
                        checkpoint_id.into(),
                        native::CAPTURE.into(),
                        "a".repeat(40).into(),
                        "b".repeat(40).into(),
                        "c".repeat(40).into(),
                        "d".repeat(40).into(),
                    ],
                ))
                .await
                .expect("seed actual-schema legacy checkpoint");
            self.conn
                .execute_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "INSERT INTO agent_coverage_revision
                     (session_id, logical_turn_key, coverage_schema_version, revision,
                      checkpoint_id, coverage_digest, completeness, source_channel, created_at)
                     VALUES (?, 'legacy-turn', 1, 1, ?, ?, 'complete', 'live', 1)",
                    [
                        native::CAPTURE.into(),
                        checkpoint_id.into(),
                        "e".repeat(64).into(),
                    ],
                ))
                .await
                .expect("seed nonempty legacy v1 revision set");
        }

        fn reader(&self) -> ReasoningIdentityReadContext {
            use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
            ReasoningIdentityReadContext::new(
                &self.scope,
                native::CAPTURE,
                &self.dir.path().join(crate::utils::util::ROOT_DIR),
                self.dir.path(),
                Instant::now() + Duration::from_secs(30),
            )
        }

        pub(crate) async fn plan(
            &self,
            turns: &[NormalizedTurn],
            fields: Vec<Vec<u8>>,
        ) -> ReasoningCoveragePlan {
            use crate::internal::ai::{
                capture::checkpoint::{
                    ReasoningIdentityPreparation, prepare_reasoning_coverage_plan,
                    reasoning_coverage_test_payload,
                },
                observed_agents::reasoning::carrier_test_support as native,
            };
            let payload = reasoning_coverage_test_payload(fields);
            prepare_reasoning_coverage_plan(
                &payload,
                ReasoningIdentityPreparation {
                    conn: &self.conn,
                    scope: &self.scope,
                    storage_path: &self.dir.path().join(crate::utils::util::ROOT_DIR),
                    authorized_root: self.dir.path(),
                    session_id: native::CAPTURE,
                    provider: "claude_code",
                    turns,
                    deadline: Instant::now() + Duration::from_secs(30),
                },
            )
            .await
            .unwrap()
            .unwrap()
        }

        pub(crate) async fn reserve(
            &self,
            turns: &[NormalizedTurn],
            plan: Option<&ReasoningCoveragePlan>,
            owner: &str,
        ) -> ReasoningReservationOutcome {
            self.try_reserve(turns, plan, owner).await.unwrap()
        }

        pub(crate) async fn try_reserve(
            &self,
            turns: &[NormalizedTurn],
            plan: Option<&ReasoningCoveragePlan>,
            owner: &str,
        ) -> Result<ReasoningReservationOutcome> {
            use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
            let reader = ReasoningIdentityReadContext::new(
                &self.scope,
                native::CAPTURE,
                &self.dir.path().join(crate::utils::util::ROOT_DIR),
                self.dir.path(),
                Instant::now() + Duration::from_secs(30),
            );
            let input = match plan {
                Some(plan) => ReasoningCoverageInput::native(plan),
                None => ReasoningCoverageInput::ordinary(&reader),
            };
            reserve_turn_claims_with_reasoning(
                &self.conn,
                &self.scope,
                native::CAPTURE,
                turns,
                owner,
                1,
                CaptureReservationExecution::live(
                    CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
                ),
                input,
            )
            .await
        }

        pub(crate) async fn commit(
            &self,
            claims: &[ReservedTurnClaim],
            plan: &ReasoningCoveragePlan,
            owner: &str,
            checkpoint: &str,
        ) {
            use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
            let companion = ReasoningClaimCommitPlan::new(
                LiveClaimCommitPlan {
                    source_channel: "live",
                    session_id: native::CAPTURE.to_owned(),
                    checkpoint_id: checkpoint.to_owned(),
                    owner: owner.to_owned(),
                    parent_commit: None,
                    created_at: 1,
                    now_ms: 1,
                    claims: claims.to_vec(),
                    import_session: None,
                    import_identity: None,
                    capture_scope: Some(self.scope.clone()),
                },
                plan,
            )
            .unwrap();
            let txn = sea_orm::TransactionTrait::begin(&self.conn).await.unwrap();
            companion
                .apply(
                    &txn,
                    &TracesCommitCtx {
                        commit_hash: format!("commit-{checkpoint}"),
                        tree_oid: "t".to_owned(),
                        metadata_blob_oid: "m".to_owned(),
                    },
                )
                .await
                .unwrap();
            txn.commit().await.unwrap();
        }

        pub(crate) async fn identity_text(&self) -> String {
            use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
            MetadataKv::get_with_conn(
                &self.conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMap::KEY,
            )
            .await
            .unwrap()
            .unwrap()
            .value
        }

        pub(crate) async fn identity_count(&self) -> i64 {
            scalar(
                &self.conn,
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_reasoning_identity'",
            )
            .await
        }
    }

    // These synthetic maps test the private carrier. Only the checkpoint
    // adapter may produce native-source evidence for a production claim.
    #[cfg(unix)]
    async fn cov2_seed_signed_private_map(
        fixture: &ReasoningFixture,
        version: i64,
        logical_turn_key: &str,
        checkpoint_id: &str,
    ) -> String {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let reader = fixture.reader();
        let slot = reader
            .turn_slot_for_schema_version(&fixture.conn, version, logical_turn_key)
            .await
            .unwrap();
        let body = ReasoningIdentityRecordBody {
            context: ReasoningIdentityRecordContext {
                repository_id: fixture.scope.repo_id.clone(),
                session_id: native::CAPTURE.to_owned(),
                logical_turn_key: slot.clone(),
                coverage_schema_version: version,
                claim_revision: 1,
                claim_fence_token: 1,
                checkpoint_id: checkpoint_id.to_owned(),
            },
            kind: ReasoningIdentityKind::Empty,
            descriptor_mac: slot.clone(),
        };
        let (key, value) = match version {
            1 => {
                let body_bytes = ReasoningIdentityRecord::authentication_body(&body).unwrap();
                let record_mac = reader
                    .authenticate_record_for_schema_version(
                        &fixture.conn,
                        version,
                        &body_bytes,
                        None,
                    )
                    .await
                    .unwrap();
                let record = ReasoningIdentityRecord::from_body_and_mac(body, record_mac).unwrap();
                let records = std::collections::BTreeMap::from([(slot, record)]);
                let map_body = ReasoningIdentitySessionMap::authentication_body(
                    &fixture.scope.repo_id,
                    native::CAPTURE,
                    &records,
                )
                .unwrap();
                let map_mac = reader
                    .authenticate_map_for_schema_version(&fixture.conn, version, &map_body, None)
                    .await
                    .unwrap();
                let map = ReasoningIdentitySessionMap::from_records_and_mac(
                    fixture.scope.repo_id.clone(),
                    native::CAPTURE.to_owned(),
                    records,
                    map_mac,
                )
                .unwrap();
                (ReasoningIdentitySessionMap::KEY, map.encode().unwrap())
            }
            2 => {
                let body_bytes = ReasoningIdentityRecordV2::authentication_body(&body).unwrap();
                let record_mac = reader
                    .authenticate_record_for_schema_version(
                        &fixture.conn,
                        version,
                        &body_bytes,
                        None,
                    )
                    .await
                    .unwrap();
                let record =
                    ReasoningIdentityRecordV2::from_body_and_mac(body, record_mac).unwrap();
                let records = std::collections::BTreeMap::from([(slot, record)]);
                let map_body = ReasoningIdentitySessionMapV2::authentication_body(
                    &fixture.scope.repo_id,
                    native::CAPTURE,
                    &records,
                )
                .unwrap();
                let map_mac = reader
                    .authenticate_map_for_schema_version(&fixture.conn, version, &map_body, None)
                    .await
                    .unwrap();
                let map = ReasoningIdentitySessionMapV2::from_records_and_mac(
                    fixture.scope.repo_id.clone(),
                    native::CAPTURE.to_owned(),
                    records,
                    map_mac,
                )
                .unwrap();
                (ReasoningIdentitySessionMapV2::KEY, map.encode().unwrap())
            }
            _ => panic!("test only accepts two known versions"),
        };
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            key,
            &value,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        value
    }

    #[cfg(unix)]
    async fn cov2_seed_bulk_signed_private_map(
        fixture: &ReasoningFixture,
        version: i64,
        count: usize,
    ) -> String {
        use crate::internal::ai::{
            capture::key::ReasoningIdentityMacPurpose,
            observed_agents::reasoning::carrier_test_support as native,
        };
        let descriptor =
            ReasoningIdentityMacPurpose::descriptor_for_schema_version(version).unwrap();
        let record_purpose =
            ReasoningIdentityMacPurpose::record_for_schema_version(version).unwrap();
        let fake_record_mac = format!("{}{}", record_purpose.prefix(), "a".repeat(64));
        let value = match version {
            1 => {
                let mut records = std::collections::BTreeMap::new();
                for index in 0..count {
                    let slot = format!("{}{index:064x}", descriptor.prefix());
                    let body = ReasoningIdentityRecordBody {
                        context: ReasoningIdentityRecordContext {
                            repository_id: fixture.scope.repo_id.clone(),
                            session_id: native::CAPTURE.to_owned(),
                            logical_turn_key: slot.clone(),
                            coverage_schema_version: 1,
                            claim_revision: 1,
                            claim_fence_token: 1,
                            checkpoint_id: "synthetic-bulk-checkpoint".to_owned(),
                        },
                        kind: ReasoningIdentityKind::Empty,
                        descriptor_mac: slot.clone(),
                    };
                    records.insert(
                        slot,
                        ReasoningIdentityRecord::from_body_and_mac(body, fake_record_mac.clone())
                            .unwrap(),
                    );
                }
                let map_body = ReasoningIdentitySessionMap::authentication_body(
                    &fixture.scope.repo_id,
                    native::CAPTURE,
                    &records,
                )
                .unwrap();
                let map_mac = fixture
                    .reader()
                    .authenticate_map_for_schema_version(&fixture.conn, 1, &map_body, None)
                    .await
                    .unwrap();
                ReasoningIdentitySessionMap::from_records_and_mac(
                    fixture.scope.repo_id.clone(),
                    native::CAPTURE.to_owned(),
                    records,
                    map_mac,
                )
                .unwrap()
                .encode()
                .unwrap()
            }
            2 => {
                let mut records = std::collections::BTreeMap::new();
                for index in 0..count {
                    let slot = format!("{}{index:064x}", descriptor.prefix());
                    let body = ReasoningIdentityRecordBody {
                        context: ReasoningIdentityRecordContext {
                            repository_id: fixture.scope.repo_id.clone(),
                            session_id: native::CAPTURE.to_owned(),
                            logical_turn_key: slot.clone(),
                            coverage_schema_version: 2,
                            claim_revision: 1,
                            claim_fence_token: 1,
                            checkpoint_id: "synthetic-bulk-checkpoint".to_owned(),
                        },
                        kind: ReasoningIdentityKind::Empty,
                        descriptor_mac: slot.clone(),
                    };
                    records.insert(
                        slot,
                        ReasoningIdentityRecordV2::from_body_and_mac(body, fake_record_mac.clone())
                            .unwrap(),
                    );
                }
                let map_body = ReasoningIdentitySessionMapV2::authentication_body(
                    &fixture.scope.repo_id,
                    native::CAPTURE,
                    &records,
                )
                .unwrap();
                let map_mac = fixture
                    .reader()
                    .authenticate_map_for_schema_version(&fixture.conn, 2, &map_body, None)
                    .await
                    .unwrap();
                ReasoningIdentitySessionMapV2::from_records_and_mac(
                    fixture.scope.repo_id.clone(),
                    native::CAPTURE.to_owned(),
                    records,
                    map_mac,
                )
                .unwrap()
                .encode()
                .unwrap()
            }
            _ => panic!("test only accepts two known versions"),
        };
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            if version == 1 {
                ReasoningIdentitySessionMap::KEY
            } else {
                ReasoningIdentitySessionMapV2::KEY
            },
            &value,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        value
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_dual_private_map_same_key_coexists() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let fixture = ReasoningFixture::new().await;
        let first = turn("synthetic-turn-0", "ordinary", Completeness::Complete);
        let first_plan = fixture
            .plan(
                std::slice::from_ref(&first),
                vec![b"synthetic source".to_vec()],
            )
            .await;
        let first_reserved = fixture
            .reserve(std::slice::from_ref(&first), Some(&first_plan), "v1-owner")
            .await;
        fixture
            .commit(
                &first_reserved.ordinary.reserved,
                &first_plan,
                "v1-owner",
                "cp-v1",
            )
            .await;
        let v1 = fixture.identity_text().await;
        let v2 = cov2_seed_signed_private_map(&fixture, 2, "synthetic-turn-0", "cp-v2").await;
        let maps = read_reasoning_map(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            Some(&fixture.reader()),
        )
        .await
        .unwrap();
        let StoredReasoningMap::Verified(maps) = maps else {
            panic!("both private maps must authenticate");
        };
        let v1_slot = fixture
            .reader()
            .turn_slot(&fixture.conn, "synthetic-turn-0")
            .await
            .unwrap();
        let v2_slot = fixture
            .reader()
            .turn_slot_for_schema_version(&fixture.conn, 2, "synthetic-turn-0")
            .await
            .unwrap();
        assert_ne!(v1_slot, v2_slot, "logical keys are domain separated");
        assert_eq!(maps.v1.as_ref().unwrap().records().len(), 1);
        assert_eq!(maps.v2.as_ref().unwrap().records().len(), 1);
        assert!(maps.v1.as_ref().unwrap().records().contains_key(&v1_slot));
        assert!(maps.v2.as_ref().unwrap().records().contains_key(&v2_slot));
        assert_eq!(fixture.identity_text().await, v1);
        let second = turn(
            "synthetic-turn-1",
            "ordinary second",
            Completeness::Complete,
        );
        let second_plan = fixture
            .plan(&[first, second.clone()], vec![b"synthetic source".to_vec()])
            .await;
        let second_reserved = fixture
            .reserve(
                std::slice::from_ref(&second),
                Some(&second_plan),
                "v1-owner-2",
            )
            .await;
        fixture
            .commit(
                &second_reserved.ordinary.reserved,
                &second_plan,
                "v1-owner-2",
                "cp-v1-second",
            )
            .await;
        let revised_v1 = fixture.identity_text().await;
        assert_ne!(revised_v1, v1);
        assert_eq!(
            ReasoningIdentitySessionMap::decode(&revised_v1)
                .unwrap()
                .records()
                .len(),
            2
        );
        assert_eq!(
            MetadataKv::get_with_conn(
                &fixture.conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMapV2::KEY,
            )
            .await
            .unwrap()
            .unwrap()
            .value,
            v2,
            "v1 claim/ref commit cannot rewrite the parallel v2 key"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_private_map_aggregate_entry_and_byte_caps() {
        let max_entries = crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES;
        let max_bytes = crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES;
        for (entries, bytes, accepts) in [
            (max_entries - 1, max_bytes - 1, true),
            (max_entries, max_bytes, true),
            (max_entries + 1, max_bytes, false),
            (max_entries, max_bytes + 1, false),
        ] {
            assert_eq!(
                validate_reasoning_map_aggregate_capacity(entries, bytes).is_ok(),
                accepts,
                "aggregate boundary entries={entries}, bytes={bytes}"
            );
        }
        // A genuine two-map read must enforce the same shared ceiling, and
        // the v1 writer must leave the retained v2 bytes untouched.
        let fixture = ReasoningFixture::new().await;
        let v1 = cov2_seed_signed_private_map(&fixture, 1, "same", "cp-v1").await;
        let v2 = cov2_seed_signed_private_map(&fixture, 2, "same", "cp-v2").await;
        assert!(validate_reasoning_map_aggregate_capacity(2, v1.len() + v2.len()).is_ok());
        assert!(matches!(
            read_reasoning_map(
                &fixture.conn,
                &fixture.scope,
                crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
                Some(&fixture.reader()),
            )
            .await
            .unwrap(),
            StoredReasoningMap::Verified(_)
        ));
        let bulk = ReasoningFixture::new().await;
        let large_v1 = cov2_seed_bulk_signed_private_map(&bulk, 1, 1500).await;
        let large_v2 = cov2_seed_bulk_signed_private_map(&bulk, 2, 1500).await;
        assert!(large_v1.len() <= max_bytes && large_v2.len() <= max_bytes);
        assert!(large_v1.len() + large_v2.len() > max_bytes);
        assert!(matches!(
            read_reasoning_map(
                &bulk.conn,
                &bulk.scope,
                crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
                Some(&bulk.reader()),
            )
            .await
            .unwrap(),
            StoredReasoningMap::Invalid
        ));
        assert!(
            reserve_turn_claims_with_reasoning_no_deadline(
                &bulk.conn,
                &bulk.scope,
                crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
                &[turn("bulk-rejected", "ordinary", Completeness::Complete)],
                "owner",
                1,
                "live",
                ReasoningCoverageInput::ordinary(&bulk.reader()),
            )
            .await
            .is_err()
        );
        assert_eq!(
            scalar(&bulk.conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
            0
        );

        // Cross the cap in the real v1 companion, rather than proving only
        // the pure bound helper or rejecting an already-invalid stored pair.
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let crossing = ReasoningFixture::new().await;
        let candidate_turn = turn("synthetic-turn-0", "ordinary", Completeness::Complete);
        let source = crossing
            .plan(
                std::slice::from_ref(&candidate_turn),
                vec![b"synthetic-capacity-source".to_vec()],
            )
            .await;
        let reservation = crossing
            .reserve(
                std::slice::from_ref(&candidate_turn),
                Some(&source),
                "capacity-owner",
            )
            .await;
        assert_eq!(reservation.ordinary.reserved.len(), 1);
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: native::CAPTURE.to_owned(),
            checkpoint_id: "capacity-cross-checkpoint".to_owned(),
            owner: "capacity-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: reservation.ordinary.reserved.clone(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(crossing.scope.clone()),
        };
        let companion = ReasoningClaimCommitPlan::new(ordinary, &source)
            .expect("bind exact v1 source to reserved claim");
        let ctx = TracesCommitCtx {
            commit_hash: "c".repeat(40),
            tree_oid: "d".repeat(40),
            metadata_blob_oid: "e".repeat(40),
        };
        let txn = crate::internal::db::begin_write_transaction(&crossing.conn)
            .await
            .expect("begin size measurement transaction");
        TracesTxnExtra::apply(&companion, &txn, &ctx)
            .await
            .expect("one v1 record fits before adding v2");
        let new_v1 = MetadataKv::get_with_conn(
            &txn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
        )
        .await
        .expect("read staged v1 map")
        .expect("staged v1 map exists")
        .value;
        txn.rollback().await.expect("revert size measurement");

        // The fixed-width slot/record encoding makes every additional
        // synthetic v2 record the same byte increment. Choose the smallest
        // individually valid v2 map that makes the new v1 envelope cross 1 MiB.
        let one = cov2_seed_bulk_signed_private_map(&crossing, 2, 1).await;
        let two = cov2_seed_bulk_signed_private_map(&crossing, 2, 2).await;
        let per_entry = two.len() - one.len();
        assert!(per_entry > 0 && new_v1.len() > per_entry);
        let base = one.len() - per_entry;
        let count = (max_bytes - new_v1.len() - base) / per_entry + 1;
        assert!(count <= max_entries);
        let retained_v2 = cov2_seed_bulk_signed_private_map(&crossing, 2, count).await;
        assert!(retained_v2.len() <= max_bytes);
        assert!(retained_v2.len() + new_v1.len() > max_bytes);
        assert!(matches!(
            read_reasoning_map(
                &crossing.conn,
                &crossing.scope,
                native::CAPTURE,
                Some(&crossing.reader()),
            )
            .await
            .expect("read pre-crossing map"),
            StoredReasoningMap::Verified(_)
        ));
        crossing
            .conn
            .execute_unprepared("CREATE TABLE cov2_capacity_ref_probe (oid TEXT NOT NULL)")
            .await
            .expect("create ref transaction rollback probe");
        crossing
            .conn
            .execute_unprepared("INSERT INTO cov2_capacity_ref_probe VALUES ('old')")
            .await
            .expect("seed ref transaction rollback probe");
        let claim_before = claim_row(&crossing.conn, "synthetic-turn-0").await;
        let txn = crate::internal::db::begin_write_transaction(&crossing.conn)
            .await
            .expect("begin real companion transaction");
        txn.execute_unprepared("UPDATE cov2_capacity_ref_probe SET oid = 'new'")
            .await
            .expect("stage ref probe");
        let capacity_error = TracesTxnExtra::apply(&companion, &txn, &ctx)
            .await
            .expect_err("new v1 record must fail the combined persisted-byte ceiling");
        assert!(
            capacity_error.is::<crate::internal::metadata::ReasoningIdentityMapCapacity>(),
            "the failure must be the combined private-map capacity"
        );
        txn.rollback().await.expect("rollback over-cap companion");
        assert_eq!(
            claim_row(&crossing.conn, "synthetic-turn-0").await,
            claim_before
        );
        assert_eq!(
            scalar(
                &crossing.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision
                 WHERE checkpoint_id = 'capacity-cross-checkpoint'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &crossing.conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE checkpoint_id = 'capacity-cross-checkpoint'"
            )
            .await,
            0
        );
        assert!(
            MetadataKv::get_with_conn(
                &crossing.conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMap::KEY,
            )
            .await
            .expect("read rolled back v1 map")
            .is_none()
        );
        assert_eq!(
            MetadataKv::get_with_conn(
                &crossing.conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMapV2::KEY,
            )
            .await
            .expect("read retained v2 map")
            .expect("retained v2 map exists")
            .value,
            retained_v2
        );
        let ref_row = crossing
            .conn
            .query_one_raw(Statement::from_string(
                crossing.conn.get_database_backend(),
                "SELECT oid FROM cov2_capacity_ref_probe".to_owned(),
            ))
            .await
            .expect("read ref rollback probe")
            .expect("ref rollback row");
        assert_eq!(ref_row.try_get_by::<String, _>("oid").unwrap(), "old");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_private_map_failure_zero_claim() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let fixture = ReasoningFixture::new().await;
        let original = turn("zero-claim", "original", Completeness::Complete);
        let reserved = fixture
            .reserve(std::slice::from_ref(&original), None, "owner")
            .await;
        let claim = &reserved.ordinary.reserved[0];
        fixture.conn.execute_raw(Statement::from_sql_and_values(
            fixture.conn.get_database_backend(),
            "UPDATE agent_coverage_claim SET state = 'catalog_committed', revision = ?, owner = NULL,
                    lease_expires_at = NULL, checkpoint_id = 'synthetic-committed'
             WHERE session_id = ? AND logical_turn_key = ?",
            [claim.next_revision.into(), native::CAPTURE.into(), claim.logical_turn_key.clone().into()],
        )).await.unwrap();
        let baseline_state = claim_row(&fixture.conn, "zero-claim").await;
        let baseline_revisions = scalar(
            &fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_revision",
        )
        .await;
        let baseline_conflicts = scalar(
            &fixture.conn,
            "SELECT COUNT(*) AS n FROM agent_coverage_conflict",
        )
        .await;
        let valid_v1 =
            cov2_seed_signed_private_map(&fixture, 1, "zero-claim", "synthetic-committed").await;
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            "unexpected-private-key",
            "hidden-canary",
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let challenger = turn("zero-claim", "changed", Completeness::Complete);
        let result = reserve_turn_claims_with_reasoning_no_deadline(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            &[challenger],
            "challenger",
            2,
            "live",
            ReasoningCoverageInput::ordinary(&fixture.reader()),
        )
        .await;
        assert!(
            result.is_err(),
            "unknown private key must reject before claim writes"
        );
        assert_eq!(claim_row(&fixture.conn, "zero-claim").await, baseline_state);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision"
            )
            .await,
            baseline_revisions
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_conflict"
            )
            .await,
            baseline_conflicts
        );
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "DELETE FROM metadata_kv WHERE scope = ? AND target = ? AND key = ?",
                [
                    MetadataScope::AgentReasoningIdentity.as_str().into(),
                    native::CAPTURE.into(),
                    "unexpected-private-key".into(),
                ],
            ))
            .await
            .unwrap();
        for variant in [
            "blob-value",
            "blob-declared-type",
            "oversized-declared-type",
        ] {
            let (sql, values): (&str, Vec<sea_orm::Value>) = match variant {
                "blob-value" => (
                    "UPDATE metadata_kv SET value = CAST(value AS BLOB)
                     WHERE scope = ? AND target = ? AND key = ?",
                    vec![
                        MetadataScope::AgentReasoningIdentity.as_str().into(),
                        native::CAPTURE.into(),
                        ReasoningIdentitySessionMap::KEY.into(),
                    ],
                ),
                "blob-declared-type" => (
                    "UPDATE metadata_kv SET value_type = CAST(x'74657874' AS BLOB)
                     WHERE scope = ? AND target = ? AND key = ?",
                    vec![
                        MetadataScope::AgentReasoningIdentity.as_str().into(),
                        native::CAPTURE.into(),
                        ReasoningIdentitySessionMap::KEY.into(),
                    ],
                ),
                _ => (
                    "UPDATE metadata_kv SET value_type = ?
                     WHERE scope = ? AND target = ? AND key = ?",
                    vec![
                        "x".repeat(1024).into(),
                        MetadataScope::AgentReasoningIdentity.as_str().into(),
                        native::CAPTURE.into(),
                        ReasoningIdentitySessionMap::KEY.into(),
                    ],
                ),
            };
            fixture
                .conn
                .execute_raw(Statement::from_sql_and_values(
                    fixture.conn.get_database_backend(),
                    sql,
                    values,
                ))
                .await
                .expect("seed noncanonical private map SQL type");
            assert!(
                reserve_turn_claims_with_reasoning_no_deadline(
                    &fixture.conn,
                    &fixture.scope,
                    native::CAPTURE,
                    &[turn("zero-claim", "changed type", Completeness::Complete)],
                    "type-challenger",
                    3,
                    "live",
                    ReasoningCoverageInput::ordinary(&fixture.reader()),
                )
                .await
                .is_err(),
                "{variant} must reject before loading map text or mutating claim"
            );
            assert_eq!(claim_row(&fixture.conn, "zero-claim").await, baseline_state);
            assert_eq!(
                scalar(
                    &fixture.conn,
                    "SELECT COUNT(*) AS n FROM agent_coverage_revision"
                )
                .await,
                baseline_revisions
            );
            assert_eq!(
                scalar(
                    &fixture.conn,
                    "SELECT COUNT(*) AS n FROM agent_coverage_conflict"
                )
                .await,
                baseline_conflicts
            );
            fixture
                .conn
                .execute_raw(Statement::from_sql_and_values(
                    fixture.conn.get_database_backend(),
                    "UPDATE metadata_kv SET value = ?, value_type = 'text'
                     WHERE scope = ? AND target = ? AND key = ?",
                    [
                        valid_v1.clone().into(),
                        MetadataScope::AgentReasoningIdentity.as_str().into(),
                        native::CAPTURE.into(),
                        ReasoningIdentitySessionMap::KEY.into(),
                    ],
                ))
                .await
                .expect("restore canonical private map after incompatible SQL type");
        }
        let mut corrupt_mac: serde_json::Value = serde_json::from_str(&valid_v1).unwrap();
        corrupt_mac["mac"] = format!("reasoning-record/hmac-v1/{}", "0".repeat(64)).into();
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
            &corrupt_mac.to_string(),
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(
            reserve_turn_claims_with_reasoning_no_deadline(
                &fixture.conn,
                &fixture.scope,
                native::CAPTURE,
                &[turn("zero-claim", "changed again", Completeness::Complete)],
                "challenger-2",
                3,
                "live",
                ReasoningCoverageInput::ordinary(&fixture.reader()),
            )
            .await
            .is_err(),
            "incorrect whole-map MAC must reject before claim writes"
        );
        assert_eq!(claim_row(&fixture.conn, "zero-claim").await, baseline_state);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_conflict"
            )
            .await,
            baseline_conflicts
        );
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "UPDATE metadata_kv SET value = ? WHERE scope = ? AND target = ? AND key = ?",
                [
                    "x".repeat(
                        crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES + 1,
                    )
                    .into(),
                    MetadataScope::AgentReasoningIdentity.as_str().into(),
                    native::CAPTURE.into(),
                    ReasoningIdentitySessionMap::KEY.into(),
                ],
            ))
            .await
            .unwrap();
        assert!(
            reserve_turn_claims_with_reasoning_no_deadline(
                &fixture.conn,
                &fixture.scope,
                native::CAPTURE,
                &[turn("zero-claim", "changed third", Completeness::Complete)],
                "challenger-3",
                4,
                "live",
                ReasoningCoverageInput::ordinary(&fixture.reader()),
            )
            .await
            .is_err(),
            "oversized private map must reject before allocating its text"
        );
        assert_eq!(claim_row(&fixture.conn, "zero-claim").await, baseline_state);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision"
            )
            .await,
            baseline_revisions
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_conflict"
            )
            .await,
            baseline_conflicts
        );
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "UPDATE metadata_kv SET value = ? WHERE scope = ? AND target = ? AND key = ?",
                [
                    valid_v1.clone().into(),
                    MetadataScope::AgentReasoningIdentity.as_str().into(),
                    native::CAPTURE.into(),
                    ReasoningIdentitySessionMap::KEY.into(),
                ],
            ))
            .await
            .expect("restore canonical private map after oversized SQL value");
        let key_path = fixture
            .dir
            .path()
            .join(crate::utils::util::ROOT_DIR)
            .join("private/agent-capture-dedup-v1.key");
        std::fs::remove_file(key_path).unwrap();
        assert!(
            reserve_turn_claims_with_reasoning_no_deadline(
                &fixture.conn,
                &fixture.scope,
                native::CAPTURE,
                &[turn(
                    "zero-claim",
                    "changed without key",
                    Completeness::Complete
                )],
                "challenger-4",
                5,
                "live",
                ReasoningCoverageInput::ordinary(&fixture.reader()),
            )
            .await
            .is_err(),
            "missing private key cannot authenticate either map"
        );
        assert_eq!(claim_row(&fixture.conn, "zero-claim").await, baseline_state);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_conflict"
            )
            .await,
            baseline_conflicts
        );

        let db_fault = ReasoningFixture::new().await;
        let prior_v1 = cov2_seed_signed_private_map(&db_fault, 1, "existing", "cp-v1").await;
        let prior_v2 = cov2_seed_signed_private_map(&db_fault, 2, "existing", "cp-v2").await;
        db_fault
            .conn
            .execute_unprepared(
                "CREATE TRIGGER cov2_reject_private_map_claim BEFORE INSERT ON agent_coverage_claim
             BEGIN SELECT RAISE(ABORT, 'injected claim write failure'); END",
            )
            .await
            .unwrap();
        assert!(
            reserve_turn_claims_with_reasoning_no_deadline(
                &db_fault.conn,
                &db_fault.scope,
                native::CAPTURE,
                &[turn("db-fault", "ordinary", Completeness::Complete)],
                "owner",
                1,
                "live",
                ReasoningCoverageInput::ordinary(&db_fault.reader()),
            )
            .await
            .is_err(),
            "SQL failure must roll back the claim attempt"
        );
        assert_eq!(
            scalar(
                &db_fault.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &db_fault.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &db_fault.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_conflict"
            )
            .await,
            0
        );
        for (key, prior) in [
            (ReasoningIdentitySessionMap::KEY, prior_v1),
            (ReasoningIdentitySessionMapV2::KEY, prior_v2),
        ] {
            assert_eq!(
                MetadataKv::get_with_conn(
                    &db_fault.conn,
                    MetadataScope::AgentReasoningIdentity,
                    native::CAPTURE,
                    key,
                )
                .await
                .expect("read retained private map after SQL fault")
                .expect("retained private map exists")
                .value,
                prior
            );
        }
        db_fault
            .conn
            .execute_unprepared("DROP TABLE metadata_kv")
            .await
            .unwrap();
        assert!(
            reserve_turn_claims_with_reasoning_no_deadline(
                &db_fault.conn,
                &db_fault.scope,
                native::CAPTURE,
                &[turn("db-read-fault", "ordinary", Completeness::Complete)],
                "owner",
                2,
                "live",
                ReasoningCoverageInput::ordinary(&db_fault.reader()),
            )
            .await
            .is_err(),
            "private-map SQL read failure must not publish a claim"
        );
        assert_eq!(
            scalar(
                &db_fault.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &db_fault.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &db_fault.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_conflict"
            )
            .await,
            0
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_commit_is_atomic_with_claim() {
        use crate::internal::ai::{
            capture::checkpoint::{
                CheckpointStore, CheckpointWriteOutcome, CheckpointWriteRequest,
                ReasoningIdentityPreparation, TracesCheckpointStore,
                prepare_reasoning_coverage_plan, reasoning_coverage_test_payload,
            },
            observed_agents::reasoning::carrier_test_support as native,
            traces::{CheckpointScope, TracesCoverageFence},
        };

        for channel in ["live", "export", "import"] {
            let fixture = ReasoningFixture::new().await;
            let storage = fixture.dir.path().join(crate::utils::util::ROOT_DIR);
            let turns = vec![
                turn("synthetic-turn-0", "ordinary", Completeness::Complete),
                turn("ordinary-turn-1", "ordinary tail", Completeness::Complete),
            ];
            // Synthetic admitted bytes exercise the real store transaction;
            // they are not a real-provider native-source acceptance result.
            let payload = reasoning_coverage_test_payload(vec![b"atomic private wire".to_vec()]);
            let plan = prepare_reasoning_coverage_plan(
                &payload,
                ReasoningIdentityPreparation {
                    conn: &fixture.conn,
                    scope: &fixture.scope,
                    storage_path: &storage,
                    authorized_root: fixture.dir.path(),
                    session_id: native::CAPTURE,
                    provider: "claude_code",
                    turns: &turns,
                    deadline: Instant::now() + Duration::from_secs(30),
                },
            )
            .await
            .unwrap()
            .unwrap();
            let now = Utc::now().timestamp_millis();
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap();
            let outcome = match channel {
                "live" | "export" => {
                    reserve_turn_claims_with_reasoning(
                        &fixture.conn,
                        &fixture.scope,
                        native::CAPTURE,
                        &turns,
                        "atomic-owner",
                        now,
                        if channel == "live" {
                            CaptureReservationExecution::live(deadline)
                        } else {
                            CaptureReservationExecution::export(deadline)
                        },
                        ReasoningCoverageInput::native(&plan),
                    )
                    .await
                }
                "import" => {
                    reserve_import_turn_claims_with_reasoning_until(
                        &fixture.conn,
                        &fixture.scope,
                        native::CAPTURE,
                        &turns,
                        "atomic-owner",
                        now,
                        deadline,
                        ReasoningCoverageInput::native(&plan),
                    )
                    .await
                }
                _ => unreachable!("fixed atomic coverage channel matrix"),
            }
            .unwrap();
            assert_eq!(outcome.skipped_incomplete_reasoning, 0);
            assert_eq!(outcome.ordinary.reserved.len(), 2);
            assert_eq!(
                plan.turn("ordinary-turn-1").unwrap().kind(),
                ReasoningIdentityKind::Empty
            );
            let state = reservation_state(channel).unwrap();
            let fences = outcome
                .ordinary
                .reserved
                .iter()
                .map(|claim| TracesCoverageFence {
                    coverage_schema_version: CoverageSchemaVersion::V1,
                    logical_turn_key: &claim.logical_turn_key,
                    coverage_digest: &claim.coverage_digest,
                    completeness: claim.completeness,
                    next_revision: claim.next_revision,
                    owner: "atomic-owner",
                    fence_token: claim.fence_token,
                    reservation_state: state,
                })
                .collect::<Vec<_>>();

            for (checkpoint, failure) in [
                ("atomic-identity-failure", Some("identity")),
                ("atomic-claim-failure", Some("claim")),
                ("atomic-success", None),
            ] {
                let trigger = match failure {
                    Some("identity") => Some(
                        "CREATE TRIGGER reject_atomic_identity BEFORE INSERT ON metadata_kv WHEN NEW.scope = 'agent_reasoning_identity' BEGIN SELECT RAISE(ABORT, 'injected identity failure'); END",
                    ),
                    Some("claim") => Some(
                        "CREATE TRIGGER reject_atomic_claim BEFORE INSERT ON agent_coverage_revision BEGIN SELECT RAISE(ABORT, 'injected claim failure'); END",
                    ),
                    _ => None,
                };
                if let Some(trigger) = trigger {
                    fixture.conn.execute_unprepared(trigger).await.unwrap();
                }
                let companion = ReasoningClaimCommitPlan::new(
                    LiveClaimCommitPlan {
                        source_channel: channel,
                        session_id: native::CAPTURE.to_owned(),
                        checkpoint_id: checkpoint.to_owned(),
                        owner: "atomic-owner".to_owned(),
                        parent_commit: None,
                        created_at: 1,
                        now_ms: now,
                        claims: outcome.ordinary.reserved.clone(),
                        import_session: None,
                        import_identity: None,
                        capture_scope: Some(fixture.scope.clone()),
                    },
                    &plan,
                )
                .unwrap();
                let store = TracesCheckpointStore::new(
                    &fixture.conn,
                    fixture.dir.path(),
                    native::CAPTURE,
                    checkpoint,
                    &fences,
                )
                .unwrap()
                .with_capture_scope(fixture.scope.clone());
                let request = CheckpointWriteRequest::new(
                    "atomic-replay",
                    checkpoint,
                    native::CAPTURE,
                    "claude_code",
                    None,
                    CheckpointScope::Committed,
                    store.marker_generation(),
                    None,
                    &payload,
                    Some(&companion),
                    None,
                )
                .unwrap();
                let result = store.write(request).await;
                if let Some(failure) = failure {
                    assert!(result.is_err(), "{channel}/{failure}: {result:?}");
                    for sql in [
                        "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'",
                        "SELECT COUNT(*) AS n FROM agent_checkpoint",
                        "SELECT COUNT(*) AS n FROM agent_coverage_revision",
                        "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_reasoning_identity'",
                    ] {
                        assert_eq!(
                            scalar(&fixture.conn, sql).await,
                            0,
                            "{channel}/{failure}: {sql}"
                        );
                    }
                    for key in ["synthetic-turn-0", "ordinary-turn-1"] {
                        assert_eq!(
                            claim_row(&fixture.conn, key).await,
                            (state.to_owned(), 0, Some(1))
                        );
                    }
                    fixture
                        .conn
                        .execute_unprepared(&format!("DROP TRIGGER reject_atomic_{failure}"))
                        .await
                        .unwrap();
                } else {
                    assert!(matches!(
                        result.unwrap(),
                        CheckpointWriteOutcome::Written { .. }
                    ));
                    assert_eq!(
                        scalar(
                            &fixture.conn,
                            "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'"
                        )
                        .await,
                        1
                    );
                    assert_eq!(
                        scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                        1
                    );
                    assert_eq!(
                        scalar(
                            &fixture.conn,
                            "SELECT COUNT(*) AS n FROM agent_coverage_revision"
                        )
                        .await,
                        2
                    );
                    let map = ReasoningIdentitySessionMap::decode(&fixture.identity_text().await)
                        .unwrap();
                    assert_eq!(map.records().len(), 2);
                    for key in ["synthetic-turn-0", "ordinary-turn-1"] {
                        assert_eq!(
                            claim_row(&fixture.conn, key).await,
                            ("catalog_committed".to_owned(), 1, Some(1))
                        );
                        let expected = plan.turn(key).unwrap();
                        let record = map.records().get(expected.slot()).unwrap();
                        assert_eq!(record.body().kind, expected.kind());
                        assert_eq!(record.body().context.checkpoint_id, checkpoint);
                        assert_eq!(record.body().context.claim_revision, 1);
                        assert_eq!(record.body().context.claim_fence_token, 1);
                    }
                    let repeat = fixture.reserve(&turns, Some(&plan), "later-owner").await;
                    assert_eq!(repeat.skipped_incomplete_reasoning, 0);
                    assert_eq!(repeat.ordinary.skipped_covered, 2);
                    assert!(repeat.ordinary.reserved.is_empty());
                }
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_live_then_import_uses_same_identity_and_skips_covered() {
        use crate::internal::ai::observed_agents::{
            coverage::Completeness, reasoning::carrier_test_support as native,
        };

        let fixture = ReasoningFixture::new().await;
        let turns = vec![turn(
            "synthetic-turn-0",
            "ordinary transcript turn",
            Completeness::Complete,
        )];
        let wire = b"synthetic admitted carrier bytes".to_vec();
        let live_plan = fixture.plan(&turns, vec![wire.clone()]).await;
        let live_identity = live_plan
            .turn("synthetic-turn-0")
            .expect("live plan includes its associated logical turn")
            .descriptor_mac()
            .to_owned();
        let live = fixture
            .reserve(&turns, Some(&live_plan), "live-identity-owner")
            .await;
        assert_eq!(live.skipped_incomplete_reasoning, 0);
        assert_eq!(live.ordinary.reserved.len(), 1);
        fixture
            .commit(
                &live.ordinary.reserved,
                &live_plan,
                "live-identity-owner",
                "live-identity-checkpoint",
            )
            .await;

        let import_plan = fixture.plan(&turns, vec![wire]).await;
        let import_identity = import_plan
            .turn("synthetic-turn-0")
            .expect("import plan includes its associated logical turn")
            .descriptor_mac();
        assert_eq!(live_identity, import_identity);
        assert!(
            import_plan
                .validate_for(
                    &fixture.scope,
                    native::CAPTURE,
                    &[turn(
                        "wrong-logical-turn",
                        "ordinary transcript turn",
                        Completeness::Complete,
                    )],
                )
                .is_err()
        );

        let imported = reserve_import_turn_claims_with_reasoning_until(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            &turns,
            "import-redelivery-owner",
            2,
            CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
            ReasoningCoverageInput::native(&import_plan),
        )
        .await
        .unwrap();
        assert_eq!(imported.skipped_incomplete_reasoning, 0);
        assert_eq!(imported.ordinary.skipped_covered, 1);
        assert!(imported.ordinary.reserved.is_empty());

        let changed_plan = fixture
            .plan(&turns, vec![b"changed synthetic carrier bytes".to_vec()])
            .await;
        let changed = reserve_import_turn_claims_with_reasoning_until(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            &turns,
            "import-changed-owner",
            3,
            CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
            ReasoningCoverageInput::native(&changed_plan),
        )
        .await
        .unwrap();
        assert_eq!(changed.ordinary.conflicted, 1);
        assert_eq!(changed.ordinary.skipped_covered, 0);
        assert!(changed.ordinary.reserved.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_forward_recovery_preserves_fences() {
        use crate::internal::ai::observed_agents::reasoning::{
            ReasoningSourceDisposition, ReasoningSourcePreprocessor, carrier_test_support as native,
        };

        let mut fixture = ReasoningFixture::new().await;
        let turn = turn(
            "synthetic-turn-0",
            "ordinary transcript turn",
            Completeness::Complete,
        );
        let turns = vec![turn];
        let plan = fixture
            .plan(
                &turns,
                vec![b"synthetic admitted identity before recovery".to_vec()],
            )
            .await;
        let reserved = fixture
            .reserve(&turns, Some(&plan), "recovery-source-owner")
            .await;
        assert_eq!(reserved.skipped_incomplete_reasoning, 0);
        assert_eq!(reserved.ordinary.reserved.len(), 1);
        fixture
            .commit(
                &reserved.ordinary.reserved,
                &plan,
                "recovery-source-owner",
                "recovery-source-checkpoint",
            )
            .await;
        let before_recovery = fixture.identity_text().await;

        // Re-open the actual SQLite store to model process recovery after the
        // forward-only claim/checkpoint commit. The private identity map and
        // both claim fences must remain usable without rebuilding a key or
        // replaying the provider artifact producer.
        fixture.conn.close().await.unwrap();
        let db_path = fixture
            .dir
            .path()
            .join(crate::utils::util::ROOT_DIR)
            .join(crate::utils::util::DATABASE);
        fixture.conn = Database::connect(format!("sqlite://{}", db_path.display()))
            .await
            .unwrap();
        assert_eq!(fixture.identity_text().await, before_recovery);
        let recovered = fixture
            .reserve(&turns, Some(&plan), "recovery-redelivery-owner")
            .await;
        assert_eq!(recovered.skipped_incomplete_reasoning, 0);
        assert_eq!(recovered.ordinary.skipped_covered, 1);
        assert!(recovered.ordinary.reserved.is_empty());

        // A source that still contains the synthetic candidate is stripped by
        // the authorized preprocessor, but has no native witness. That
        // recovery delivery remains partial and cannot refresh either fence.
        let preprocessor = native::Adapter(native::Mode::Strip);
        let prepared = preprocessor
            .prepare_authorized_source([b"safe ".as_slice(), native::CANARY].concat())
            .unwrap();
        let (ordinary, disposition) = ReasoningSourceDisposition::from_prepared_authorized_source(
            prepared,
            true,
            crate::internal::ai::traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES,
            "claude_code",
            native::CAPTURE,
            Some(native::SOURCE),
        )
        .unwrap();
        assert_eq!(ordinary, b"safe ");
        assert!(!disposition.is_ordinary());
        let without_native_proof = fixture
            .reserve(&turns, None, "recovery-unproven-owner")
            .await;
        assert_eq!(without_native_proof.skipped_incomplete_reasoning, 1);
        assert_eq!(without_native_proof.ordinary.skipped_covered, 0);
        assert!(without_native_proof.ordinary.reserved.is_empty());
        assert_eq!(fixture.identity_text().await, before_recovery);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_later_turn_keeps_prefix_covered() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;

        let fixture = ReasoningFixture::new().await;
        let prefix = turn("legacy-prefix", "unchanged prefix", Completeness::Complete);
        let initial = fixture
            .reserve(std::slice::from_ref(&prefix), None, "legacy-owner")
            .await;
        commit_reserved(
            &fixture.conn,
            native::CAPTURE,
            "legacy-owner",
            &initial.ordinary.reserved[0],
            "legacy-prefix-checkpoint",
        )
        .await
        .unwrap();
        let native_turn = turn("synthetic-turn-0", "later turn", Completeness::Complete);
        let before_turns = vec![prefix.clone(), native_turn.clone()];
        let before = fixture
            .plan(&before_turns, vec![b"later private wire".to_vec()])
            .await;
        let appended = fixture
            .reserve(&before_turns, Some(&before), "native-owner")
            .await;
        assert_eq!(appended.ordinary.skipped_covered, 1);
        assert_eq!(appended.ordinary.reserved.len(), 1);
        assert_eq!(
            appended.ordinary.reserved[0].logical_turn_key,
            "synthetic-turn-0"
        );
        fixture
            .commit(
                &appended.ordinary.reserved,
                &before,
                "native-owner",
                "native-checkpoint",
            )
            .await;
        let retained = fixture.identity_text().await;

        let after_turns = vec![
            prefix.clone(),
            native_turn.clone(),
            turn("append-turn", "new ordinary turn", Completeness::Complete),
        ];
        let after = fixture
            .plan(&after_turns, vec![b"later private wire".to_vec()])
            .await;
        for key in ["legacy-prefix", "synthetic-turn-0"] {
            let old = before.turn(key).unwrap();
            let new = after.turn(key).unwrap();
            assert_eq!(old.slot(), new.slot());
            assert_eq!(old.kind(), new.kind());
            assert_eq!(old.descriptor_mac(), new.descriptor_mac());
        }
        let comparable_prefix = fixture
            .reserve(
                std::slice::from_ref(&prefix),
                Some(&after),
                "prefix-native-reader",
            )
            .await;
        assert_eq!(comparable_prefix.ordinary.skipped_covered, 1);
        assert_eq!(comparable_prefix.skipped_incomplete_reasoning, 0);

        // A whole None/v1 capture can authenticate absence for this legacy
        // slot; it never invents a VerifiedEmpty candidate or backfills it.
        let ordinary_prefix = fixture
            .reserve(
                std::slice::from_ref(&prefix),
                None,
                "prefix-ordinary-reader",
            )
            .await;
        assert_eq!(ordinary_prefix.ordinary.skipped_covered, 1);
        assert_eq!(ordinary_prefix.skipped_incomplete_reasoning, 0);
        let unproven_native = fixture
            .reserve(
                std::slice::from_ref(&native_turn),
                None,
                "native-without-proof",
            )
            .await;
        assert_eq!(unproven_native.skipped_incomplete_reasoning, 1);
        assert_eq!(unproven_native.ordinary.skipped_covered, 0);
        assert!(unproven_native.ordinary.reserved.is_empty());
        let mixed = fixture
            .reserve(&after_turns, None, "mixed-ordinary-reader")
            .await;
        assert_eq!(mixed.skipped_incomplete_reasoning, 1);
        assert!(mixed.ordinary.reserved.is_empty());
        assert_eq!(fixture.identity_text().await, retained);
        let map = ReasoningIdentitySessionMap::decode(&retained).unwrap();
        assert_eq!(map.records().len(), 1);
        assert!(
            !map.records()
                .contains_key(before.turn("legacy-prefix").unwrap().slot())
        );
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            2
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM object_index").await,
            0
        );
        assert_eq!(
            claim_row(&fixture.conn, "legacy-prefix").await,
            ("catalog_committed".to_owned(), 1, Some(1))
        );

        let storage = fixture.dir.path().join(crate::utils::util::ROOT_DIR);
        let key_file = storage.join("private/agent-capture-dedup-v1.key");
        std::fs::remove_file(&key_file).unwrap();
        let reader = ReasoningIdentityReadContext::new(
            &fixture.scope,
            native::CAPTURE,
            &storage,
            fixture.dir.path(),
            Instant::now() + Duration::from_secs(30),
        );
        let missing_key = reserve_turn_claims_with_reasoning(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            std::slice::from_ref(&prefix),
            "missing-key-ordinary-reader",
            1,
            CaptureReservationExecution::live(
                CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
            ),
            ReasoningCoverageInput::ordinary(&reader),
        )
        .await
        .err()
        .expect("ordinary identity readers cannot recreate a key");
        assert!(format!("{missing_key:#}").contains("key is missing"));
        assert!(!key_file.exists());
        assert_eq!(fixture.identity_text().await, retained);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            2
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_empty_identity_preserves_coverage_digest() {
        let fixture = ReasoningFixture::new().await;
        let turn = turn(
            "legacy-no-candidate",
            "ordinary turn without a reasoning candidate",
            Completeness::Complete,
        );
        let previous_digest =
            crate::internal::ai::observed_agents::coverage::coverage_digest_hex(&turn.records);

        let reserved = fixture
            .reserve(std::slice::from_ref(&turn), None, "legacy-digest-owner")
            .await;

        assert_eq!(reserved.skipped_incomplete_reasoning, 0);
        assert_eq!(reserved.ordinary.reserved.len(), 1);
        assert_eq!(
            reserved.ordinary.reserved[0].coverage_digest,
            previous_digest
        );
        assert_eq!(turn.digest_hex(), previous_digest);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_preserves_content_hash() {
        use crate::internal::ai::traces::{
            CHECKPOINT_CONTENT_HASH_COVERAGE, checkpoint_content_hash,
        };

        let roles = [
            b"{\"created_at\":1}".as_slice(),
            b"{}\n".as_slice(),
            b"turn\n".as_slice(),
            b"{}\n".as_slice(),
        ];
        assert_eq!(
            CHECKPOINT_CONTENT_HASH_COVERAGE,
            [
                "metadata",
                "lifecycle_events",
                "transcript",
                "redaction_report"
            ]
        );
        let legacy_hash = checkpoint_content_hash(&roles);
        assert_eq!(
            legacy_hash,
            "sha256:0f72448126247d21ba4d9c4fbfc3c46a62a3c2dc740dfec4ffeb69a8cb2df559"
        );

        let fixture = ReasoningFixture::new().await;
        let turn = turn(
            "synthetic-turn-0",
            "ordinary turn with separate private identity",
            Completeness::Complete,
        );
        let plan = fixture
            .plan(
                std::slice::from_ref(&turn),
                vec![b"synthetic private identity is metadata-only".to_vec()],
            )
            .await;
        let reserved = fixture
            .reserve(
                std::slice::from_ref(&turn),
                Some(&plan),
                "content-hash-owner",
            )
            .await;
        fixture
            .commit(
                &reserved.ordinary.reserved,
                &plan,
                "content-hash-owner",
                "content-hash-checkpoint",
            )
            .await;
        assert!(!fixture.identity_text().await.is_empty());
        assert_eq!(checkpoint_content_hash(&roles), legacy_hash);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_is_bound_to_logical_turn() {
        let fixture = ReasoningFixture::new().await;
        let keyed_turn = turn(
            "synthetic-turn-0",
            "ordinary turn bound to its key",
            Completeness::Complete,
        );
        let plan = fixture
            .plan(
                std::slice::from_ref(&keyed_turn),
                vec![b"private wire".to_vec()],
            )
            .await;

        let identity = plan.turn(&keyed_turn.logical_turn_key).unwrap();
        let other_slot = plan
            .read_context()
            .turn_slot(&fixture.conn, "other-logical-turn")
            .await
            .unwrap();
        assert_ne!(identity.slot(), other_slot);
        assert!(
            plan.validate_for(
                &fixture.scope,
                crate::internal::ai::observed_agents::reasoning::carrier_test_support::CAPTURE,
                &[turn(
                    "other-logical-turn",
                    "ordinary turn bound to its key",
                    Completeness::Complete
                )]
            )
            .is_err(),
            "a descriptor for one logical turn must not authenticate another key"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_cannot_upgrade_native_source() {
        let fixture = ReasoningFixture::new().await;
        let turn = turn(
            "synthetic-turn-0",
            "ordinary native turn",
            Completeness::Complete,
        );
        let candidate = fixture
            .plan(
                std::slice::from_ref(&turn),
                vec![b"synthetic source identity".to_vec()],
            )
            .await;
        let initial = fixture
            .reserve(
                std::slice::from_ref(&turn),
                Some(&candidate),
                "native-owner",
            )
            .await;
        fixture
            .commit(
                &initial.ordinary.reserved,
                &candidate,
                "native-owner",
                "native-checkpoint",
            )
            .await;
        let identity_before = fixture.identity_text().await;

        // A caller with only the legacy/read authority cannot manufacture a
        // comparable native identity from an existing descriptor.
        let unproven = fixture
            .reserve(std::slice::from_ref(&turn), None, "unproven-redelivery")
            .await;
        assert_eq!(unproven.skipped_incomplete_reasoning, 1);
        assert_eq!(unproven.ordinary.skipped_covered, 0);
        assert!(unproven.ordinary.reserved.is_empty());
        assert_eq!(fixture.identity_text().await, identity_before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_byte_budget_precedes_mutation() {
        use crate::internal::ai::observed_agents::reasoning::{
            ReasoningSourcePreprocessor,
            carrier_test_support::{Adapter, CAPTURE, Mode, SOURCE},
        };

        let fixture = ReasoningFixture::new().await;
        let storage = fixture.dir.path().join(crate::utils::util::ROOT_DIR);
        let key_path = storage.join("private/agent-capture-dedup-v1.key");
        let key_before = std::fs::read(&key_path).unwrap();
        let prepared = Adapter(Mode::ArtifactOversize)
            .prepare_authorized_source(b"ordinary".to_vec())
            .unwrap();
        let rejected = crate::internal::ai::observed_agents::reasoning::ReasoningSourceDisposition::from_prepared_authorized_source(
            prepared,
            true,
            crate::internal::ai::traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES,
            "claude_code",
            CAPTURE,
            Some(SOURCE),
        )
        .err()
        .expect("oversize native field must be rejected before identity preparation");
        assert_eq!(
            rejected,
            crate::internal::ai::observed_agents::reasoning::ReasoningFieldError::SourceTooLarge
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), key_before);
        assert_eq!(fixture.identity_count().await, 0);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_count_budget_precedes_mutation() {
        use crate::internal::ai::observed_agents::reasoning::{
            ReasoningSourcePreprocessor,
            carrier_test_support::{Adapter, CAPTURE, Mode, SOURCE},
        };

        let fixture = ReasoningFixture::new().await;
        let storage = fixture.dir.path().join(crate::utils::util::ROOT_DIR);
        let key_path = storage.join("private/agent-capture-dedup-v1.key");
        let key_before = std::fs::read(&key_path).unwrap();
        let prepared = Adapter(Mode::TooMany)
            .prepare_authorized_source(b"ordinary".to_vec())
            .unwrap();
        let rejected = crate::internal::ai::observed_agents::reasoning::ReasoningSourceDisposition::from_prepared_authorized_source(
            prepared,
            true,
            crate::internal::ai::traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES,
            "claude_code",
            CAPTURE,
            Some(SOURCE),
        )
        .err()
        .expect("too many native fields must be rejected before identity preparation");
        assert_eq!(
            rejected,
            crate::internal::ai::observed_agents::reasoning::ReasoningFieldError::UnsupportedShape
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), key_before);
        assert_eq!(fixture.identity_count().await, 0);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_deadline_precedes_mutation() {
        use crate::internal::ai::{
            capture::checkpoint::{
                ReasoningIdentityPreparation, prepare_reasoning_coverage_plan,
                reasoning_coverage_test_payload,
            },
            observed_agents::reasoning::carrier_test_support as native,
        };

        let fixture = ReasoningFixture::new().await;
        let turns = [turn(
            "synthetic-turn-0",
            "ordinary turn",
            Completeness::Complete,
        )];
        let payload = reasoning_coverage_test_payload(vec![b"bounded source wire".to_vec()]);
        let expired = Instant::now() - Duration::from_secs(1);
        let error = prepare_reasoning_coverage_plan(
            &payload,
            ReasoningIdentityPreparation {
                conn: &fixture.conn,
                scope: &fixture.scope,
                storage_path: &fixture.dir.path().join(crate::utils::util::ROOT_DIR),
                authorized_root: fixture.dir.path(),
                session_id: native::CAPTURE,
                provider: "claude_code",
                turns: &turns,
                deadline: expired,
            },
        )
        .await
        .err()
        .expect("an expired preparation cannot authenticate or reserve identity");
        assert!(error.to_string().contains("deadline"));
        assert_eq!(fixture.identity_count().await, 0);
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_same_complete_identity_skips_covered() {
        let fixture = ReasoningFixture::new().await;
        let turns = vec![turn(
            "synthetic-turn-0",
            "same ordinary text",
            Completeness::Complete,
        )];
        let plan = fixture.plan(&turns, vec![b"private wire".to_vec()]).await;
        let first = fixture.reserve(&turns, Some(&plan), "owner-one").await;
        assert_eq!(first.ordinary.reserved.len(), 1);
        fixture
            .commit(&first.ordinary.reserved, &plan, "owner-one", "native-one")
            .await;
        let before = fixture.identity_text().await;
        let second = fixture.reserve(&turns, Some(&plan), "owner-two").await;
        assert_eq!(second.ordinary.skipped_covered, 1);
        assert_eq!(second.skipped_incomplete_reasoning, 0);
        assert!(second.ordinary.reserved.is_empty());
        assert_eq!(fixture.identity_text().await, before);
        assert!(!before.contains("private wire"));
        assert!(!before.contains("synthetic-turn-0"));
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_changed_complete_identity_conflicts() {
        let fixture = ReasoningFixture::new().await;
        let turns = vec![turn(
            "synthetic-turn-0",
            "same ordinary text",
            Completeness::Complete,
        )];
        let before = fixture
            .plan(&turns, vec![b"first private wire".to_vec()])
            .await;
        let first = fixture.reserve(&turns, Some(&before), "owner-one").await;
        fixture
            .commit(&first.ordinary.reserved, &before, "owner-one", "native-one")
            .await;
        let original = fixture.identity_text().await;
        let changed = fixture
            .plan(&turns, vec![b"changed private wire".to_vec()])
            .await;
        let collision = fixture.reserve(&turns, Some(&changed), "owner-two").await;
        assert_eq!(collision.ordinary.conflicted, 1);
        assert_eq!(collision.ordinary.skipped_covered, 0);
        assert!(collision.ordinary.reserved.is_empty());
        assert_eq!(
            claim_row(&fixture.conn, "synthetic-turn-0").await.0,
            "conflicted"
        );
        assert_eq!(fixture.identity_text().await, original);
        let row = fixture
            .conn
            .query_one_raw(Statement::from_string(
                fixture.conn.get_database_backend(),
                "SELECT incoming_redaction_report_json FROM agent_coverage_conflict".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        let report: serde_json::Value = serde_json::from_str(
            &row.try_get_by::<String, _>("incoming_redaction_report_json")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(report["reason"], "reasoning_identity_changed");
        assert!(!report.to_string().contains("private wire"));
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_incomplete_to_complete_advances_revision() {
        let fixture = ReasoningFixture::new().await;
        let incomplete = vec![turn(
            "synthetic-turn-0",
            "ordinary",
            Completeness::Incomplete,
        )];
        let plan = fixture
            .plan(&incomplete, vec![b"first wire".to_vec()])
            .await;
        let first = fixture.reserve(&incomplete, Some(&plan), "owner-one").await;
        fixture
            .commit(&first.ordinary.reserved, &plan, "owner-one", "native-one")
            .await;
        let complete = vec![turn("synthetic-turn-0", "ordinary", Completeness::Complete)];
        let changed = fixture
            .plan(&complete, vec![b"changed wire".to_vec()])
            .await;
        let upgrade = fixture
            .reserve(&complete, Some(&changed), "owner-two")
            .await;
        assert_eq!(upgrade.ordinary.reserved.len(), 1);
        let claim = &upgrade.ordinary.reserved[0];
        assert_eq!(claim.next_revision, 2);
        assert_eq!(claim.fence_token, 2);
        assert_eq!(upgrade.skipped_incomplete_reasoning, 0);
        fixture
            .commit(
                &upgrade.ordinary.reserved,
                &changed,
                "owner-two",
                "native-two",
            )
            .await;
        assert_eq!(
            claim_row(&fixture.conn, "synthetic-turn-0").await,
            ("catalog_committed".to_owned(), 2, Some(2))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_same_incomplete_keeps_nondowngrade() {
        let fixture = ReasoningFixture::new().await;
        let complete = vec![turn("synthetic-turn-0", "ordinary", Completeness::Complete)];
        let plan = fixture.plan(&complete, vec![b"same wire".to_vec()]).await;
        let first = fixture.reserve(&complete, Some(&plan), "owner-one").await;
        fixture
            .commit(&first.ordinary.reserved, &plan, "owner-one", "native-one")
            .await;
        let incomplete = vec![turn(
            "synthetic-turn-0",
            "ordinary",
            Completeness::Incomplete,
        )];
        let second = fixture.reserve(&incomplete, Some(&plan), "owner-two").await;
        assert_eq!(second.ordinary.skipped_covered, 1);
        assert_eq!(second.skipped_incomplete_reasoning, 0);
        assert_eq!(
            claim_row(&fixture.conn, "synthetic-turn-0").await,
            ("catalog_committed".to_owned(), 1, Some(1))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_changed_incomplete_cannot_claim_covered() {
        let fixture = ReasoningFixture::new().await;
        let complete = vec![turn("synthetic-turn-0", "ordinary", Completeness::Complete)];
        let plan = fixture.plan(&complete, vec![b"first wire".to_vec()]).await;
        let first = fixture.reserve(&complete, Some(&plan), "owner-one").await;
        fixture
            .commit(&first.ordinary.reserved, &plan, "owner-one", "native-one")
            .await;
        let original = fixture.identity_text().await;
        let incomplete = vec![
            turn("synthetic-turn-0", "ordinary", Completeness::Incomplete),
            turn("later-fresh", "new ordinary", Completeness::Complete),
        ];
        let changed = fixture
            .plan(&incomplete, vec![b"changed wire".to_vec()])
            .await;
        let second = fixture
            .reserve(&incomplete, Some(&changed), "owner-two")
            .await;
        assert_eq!(second.skipped_incomplete_reasoning, 1);
        assert_eq!(second.ordinary.skipped_covered, 0);
        assert!(second.ordinary.reserved.is_empty());
        assert_eq!(
            scalar(
                &fixture.conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1
        );
        assert_eq!(fixture.identity_text().await, original);
        assert_eq!(
            claim_row(&fixture.conn, "synthetic-turn-0").await,
            ("catalog_committed".to_owned(), 1, Some(1))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_compare_observes_current_fence() {
        let fixture = ReasoningFixture::new().await;
        let turns = vec![turn("synthetic-turn-0", "ordinary", Completeness::Complete)];
        let plan = fixture.plan(&turns, vec![b"same wire".to_vec()]).await;
        let first = fixture.reserve(&turns, Some(&plan), "owner-one").await;
        fixture
            .commit(&first.ordinary.reserved, &plan, "owner-one", "native-one")
            .await;
        for column in ["revision", "fence_token"] {
            fixture
                .conn
                .execute_unprepared(&format!(
                    "UPDATE agent_coverage_claim SET {column} = {column} + 1"
                ))
                .await
                .unwrap();
            let outcome = fixture.reserve(&turns, Some(&plan), "owner-two").await;
            assert_eq!(outcome.skipped_incomplete_reasoning, 1, "{column}");
            assert_eq!(outcome.ordinary.skipped_covered, 0);
            fixture
                .conn
                .execute_unprepared(&format!(
                    "UPDATE agent_coverage_claim SET {column} = {column} - 1"
                ))
                .await
                .unwrap();
        }
        fixture
            .conn
            .execute_unprepared(
                "UPDATE agent_coverage_claim SET checkpoint_id = 'different-generation'",
            )
            .await
            .unwrap();
        let outcome = fixture.reserve(&turns, Some(&plan), "owner-two").await;
        assert_eq!(outcome.skipped_incomplete_reasoning, 1);
        assert_eq!(outcome.ordinary.skipped_covered, 0);
        fixture
            .conn
            .execute_unprepared("UPDATE agent_coverage_claim SET checkpoint_id = 'native-one'")
            .await
            .unwrap();
        fixture
            .conn
            .execute_unprepared("DELETE FROM agent_checkpoint")
            .await
            .unwrap();
        let retained = fixture.reserve(&turns, Some(&plan), "owner-two").await;
        assert_eq!(retained.skipped_incomplete_reasoning, 1);
        assert_eq!(retained.ordinary.skipped_covered, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_legacy_partial_and_invalid_identity_error_preserve_no_writes() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        for (incumbent, text, expect_conflict) in [
            (Completeness::Complete, "ordinary", false),
            (Completeness::Complete, "different ordinary", true),
            (Completeness::Incomplete, "ordinary", false),
        ] {
            let fixture = ReasoningFixture::new().await;
            let old = vec![turn("synthetic-turn-0", "ordinary", incumbent)];
            let reservation = fixture.reserve(&old, None, "legacy-owner").await;
            commit_reserved(
                &fixture.conn,
                native::CAPTURE,
                "legacy-owner",
                &reservation.ordinary.reserved[0],
                "legacy-one",
            )
            .await
            .unwrap();
            let control = fixture.reserve(&old, None, "control-owner").await;
            assert_eq!(control.ordinary.skipped_covered, 1);
            let turns = vec![turn("synthetic-turn-0", text, Completeness::Complete)];
            let plan = fixture
                .plan(&turns, vec![b"private candidate".to_vec()])
                .await;
            let result = fixture.reserve(&turns, Some(&plan), "new-owner").await;
            if expect_conflict {
                assert_eq!(result.ordinary.conflicted, 1);
                assert_eq!(result.skipped_incomplete_reasoning, 0);
            } else if incumbent == Completeness::Incomplete {
                assert_eq!(result.ordinary.reserved.len(), 1);
                assert_eq!(result.ordinary.reserved[0].next_revision, 2);
                assert_eq!(result.ordinary.reserved[0].fence_token, 2);
            } else {
                assert_eq!(result.skipped_incomplete_reasoning, 1);
                assert_eq!(result.ordinary.skipped_covered, 0);
                assert!(result.ordinary.reserved.is_empty());
            }
            assert_eq!(
                scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
                1
            );
            assert_eq!(
                scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM object_index").await,
                0
            );
            assert_eq!(
                scalar(
                    &fixture.conn,
                    "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope='agent_reasoning_identity'"
                )
                .await,
                0
            );
        }

        let fixture = ReasoningFixture::new().await;
        let turns = vec![turn("synthetic-turn-0", "ordinary", Completeness::Complete)];
        let plan = fixture.plan(&turns, vec![b"same wire".to_vec()]).await;
        let first = fixture.reserve(&turns, Some(&plan), "owner-one").await;
        fixture
            .commit(&first.ordinary.reserved, &plan, "owner-one", "native-one")
            .await;
        let original = fixture.identity_text().await;
        let without_plan = fixture.reserve(&turns, None, "no-plan-owner").await;
        assert_eq!(without_plan.skipped_incomplete_reasoning, 1);
        assert_eq!(without_plan.ordinary.skipped_covered, 0);

        for variant in 0..5 {
            let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
            match variant {
                0 => value["version"] = 2.into(),
                1 => value["unknown"] = "private payload".into(),
                2 => value["repository_id"] = "another-repository".into(),
                3 => value["mac"] = format!("reasoning-record/hmac-v1/{}", "0".repeat(64)).into(),
                _ => value = "wrong shape".into(),
            }
            MetadataKv::set_with_conn(
                &fixture.conn,
                MetadataScope::AgentReasoningIdentity,
                native::CAPTURE,
                ReasoningIdentitySessionMap::KEY,
                &value.to_string(),
                MetadataValueType::Text,
            )
            .await
            .unwrap();
            assert!(
                fixture
                    .try_reserve(&turns, Some(&plan), "bad-map-owner")
                    .await
                    .is_err(),
                "invalid private map variant {variant} must reject the whole reservation"
            );
            assert_eq!(fixture.identity_text().await, value.to_string());
            assert_eq!(
                claim_row(&fixture.conn, "synthetic-turn-0").await,
                ("catalog_committed".to_owned(), 1, Some(1))
            );
        }
        // Seed an oversized legacy/corrupt value below the metadata owner API
        // so the bounded SQL reader is tested against data that can only be
        // present after a prior buggy writer or direct database corruption.
        let oversized =
            "x".repeat(crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES + 1);
        let updated = fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "UPDATE metadata_kv SET value = ? WHERE scope = ? AND target = ? AND key = ?",
                vec![
                    oversized.clone().into(),
                    MetadataScope::AgentReasoningIdentity.as_str().into(),
                    native::CAPTURE.into(),
                    ReasoningIdentitySessionMap::KEY.into(),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(updated.rows_affected(), 1);
        assert!(
            fixture
                .try_reserve(&turns, Some(&plan), "oversized-map-owner")
                .await
                .is_err(),
            "oversized private map must reject the whole reservation"
        );
        assert_eq!(fixture.identity_text().await, oversized);
        assert_eq!(
            claim_row(&fixture.conn, "synthetic-turn-0").await,
            ("catalog_committed".to_owned(), 1, Some(1))
        );

        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentReasoningIdentity,
            native::CAPTURE,
            ReasoningIdentitySessionMap::KEY,
            &original,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let storage = fixture.dir.path().join(crate::utils::util::ROOT_DIR);
        let key_file = storage.join("private/agent-capture-dedup-v1.key");
        std::fs::remove_file(&key_file).unwrap();
        let error = reserve_turn_claims_with_reasoning(
            &fixture.conn,
            &fixture.scope,
            native::CAPTURE,
            &turns,
            "missing-key-reader",
            1,
            CaptureReservationExecution::live(
                CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
            ),
            ReasoningCoverageInput::native(&plan),
        )
        .await
        .err()
        .expect("a reader cannot repair its lost key");
        assert!(format!("{error:#}").contains("key is missing"));
        assert!(!key_file.exists());
        assert_eq!(fixture.identity_text().await, original);
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            1
        );
        assert_eq!(
            claim_row(&fixture.conn, "synthetic-turn-0").await,
            ("catalog_committed".to_owned(), 1, Some(1))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_identity_difference_survives_conflict_redaction() {
        let fixture = ReasoningFixture::new().await;
        let mut incoming = turn(
            "synthetic-turn-0",
            "key AKIAIOSFODNN7EXAMPLE",
            Completeness::Complete,
        );
        let mut incumbent = incoming.clone();
        redact_turns_with_report(std::slice::from_mut(&mut incumbent));
        assert_ne!(incumbent.digest_hex(), incoming.digest_hex());
        let original = fixture
            .plan(
                std::slice::from_ref(&incumbent),
                vec![b"first private wire".to_vec()],
            )
            .await;
        let first = fixture
            .reserve(
                std::slice::from_ref(&incumbent),
                Some(&original),
                "owner-one",
            )
            .await;
        fixture
            .commit(
                &first.ordinary.reserved,
                &original,
                "owner-one",
                "native-one",
            )
            .await;
        let changed = fixture
            .plan(
                std::slice::from_ref(&incoming),
                vec![b"changed private wire".to_vec()],
            )
            .await;
        let result = fixture
            .reserve(std::slice::from_ref(&incoming), Some(&changed), "owner-two")
            .await;
        assert_eq!(result.ordinary.conflicted, 1);
        assert_eq!(result.ordinary.skipped_covered, 0);
        redact_turns_with_report(std::slice::from_mut(&mut incoming));
        assert_eq!(incoming.digest_hex(), incumbent.digest_hex());
        let evidence = fixture
            .conn
            .query_one_raw(Statement::from_string(
                fixture.conn.get_database_backend(),
                "SELECT incoming_digest, incoming_canonical_json FROM agent_coverage_conflict"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            evidence.try_get_by::<String, _>("incoming_digest").unwrap(),
            incumbent.digest_hex()
        );
        let safe: String = evidence.try_get_by("incoming_canonical_json").unwrap();
        assert!(!safe.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!safe.contains("private wire"));
    }

    /// Codex M3 R2 P1-2: the export path must release DIRTY (retryable) only
    /// when it reserved nothing yet turns are held in flight by another writer,
    /// and must NOT confuse that with a genuine all-covered no-op.
    #[test]
    fn inflight_only_skip_distinguishes_foreign_hold_from_covered_noop() {
        // Nothing reserved, one turn held by another live writer → retry dirty.
        let foreign_hold = LiveReservationOutcome {
            skipped_inflight: 1,
            ..Default::default()
        };
        assert!(foreign_hold.is_inflight_only_skip());

        // Genuine all-covered no-op (nothing in flight) → advance honestly.
        let all_covered = LiveReservationOutcome {
            skipped_covered: 3,
            ..Default::default()
        };
        assert!(!all_covered.is_inflight_only_skip());

        // A fully empty outcome is not an in-flight skip either.
        assert!(!LiveReservationOutcome::default().is_inflight_only_skip());
    }

    #[tokio::test]
    async fn in_process_batch_delay_checks_import_deadline_before_a_second_reservation_batch() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turns = (0..65)
            .map(|ordinal| {
                turn(
                    &format!("batch-deadline-{ordinal}"),
                    "historical import coverage",
                    Completeness::Complete,
                )
            })
            .collect::<Vec<_>>();
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_millis(500),
            chrono::Utc::now().timestamp_millis().saturating_add(500),
        );
        let error = with_import_reservation_batch_delay(
            std::time::Duration::from_millis(650),
            reserve_import_turn_claims_until(
                &conn,
                &scope,
                session,
                &turns,
                "batch-deadline-owner",
                1,
                deadline,
            ),
        )
        .await
        .expect_err("the second batch must observe the elapsed import deadline");
        assert!(
            format!("{error:#}").contains("execution deadline"),
            "unexpected batch-deadline error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim \
                 WHERE session_id = 'claude_code__scope_expiry' \
                   AND logical_turn_key LIKE 'batch-deadline-%'",
            )
            .await,
            64,
            "the deadline check must prevent a partial second reservation batch"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim \
                 WHERE session_id = 'claude_code__scope_expiry' \
                   AND logical_turn_key = 'batch-deadline-64'",
            )
            .await,
            0,
            "the final turn must remain unreserved after the inter-batch deadline"
        );
    }

    #[tokio::test]
    async fn import_reservation_deadline_bounds_file_writer_acquisition_without_claim_mutation() {
        let repository = tempfile::tempdir().expect("create coverage deadline repository");
        let database_path = repository.path().join("coverage-deadline.sqlite");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("coverage deadline database path is utf-8"),
        )
        .await
        .expect("create coverage deadline database");
        let backend = conn.get_database_backend();
        conn.execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode for coverage deadline regression");
        let scope = CaptureScope {
            repo_id: "coverage-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("coverage-deadline-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/coverage-deadline-workspace', 'agent',
                       'active', 'coverage-deadline-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed coverage deadline workspace lease");
        conn.execute_unprepared(
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('coverage-deadline-session', 'claude_code', 'coverage-deadline-provider',
                       'active', '/tmp', '{}', '{}', 0, 0, 1,
                       'coverage-deadline-repo', '', 'coverage-deadline-workspace', 7, 'scoped')",
        )
        .await
        .expect("seed coverage deadline session");

        let locker = crate::internal::db::establish_connection_with_lock_timeout(
            database_path
                .to_str()
                .expect("coverage deadline database path is utf-8"),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("open coverage deadline lock holder");
        locker
            .execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode on coverage deadline lock holder");
        locker
            .execute_raw(Statement::from_string(
                backend,
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive coverage deadline lock");

        let turns = [turn(
            "coverage-deadline-turn",
            "historical coverage under a database lock",
            Completeness::Complete,
        )];
        let started = Instant::now();
        let reservation = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reserve_import_turn_claims_until(
                &conn,
                &scope,
                "coverage-deadline-session",
                &turns,
                "coverage-deadline-owner",
                1,
                CaptureCommitDeadline::from_budget(std::time::Duration::from_millis(30))
                    .expect("establish coverage reservation deadline"),
            ),
        )
        .await
        .expect("coverage writer acquisition must honor its deadline");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK".to_string()))
            .await
            .expect("release exclusive coverage deadline lock");

        let error = reservation.expect_err("locked import reservation must time out");
        assert!(
            format!("{error:#}")
                .contains("import coverage reservation exceeded its execution deadline"),
            "unexpected locked import reservation error: {error:#}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "coverage writer acquisition must not wait for SQLite's busy timeout"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'coverage-deadline-session'
                   AND logical_turn_key = 'coverage-deadline-turn'",
            )
            .await,
            0,
            "a cancelled coverage writer acquisition must not publish a delayed claim"
        );
    }

    /// A failed managed hook may use only its short recovery grace to release
    /// an existing owned claim. An independent SQLite writer lock must not
    /// leave that cleanup queued and let it abandon the claim after the grace.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandonment_recovery_grace_bounds_file_writer_acquisition_without_late_write() {
        let repository = tempfile::tempdir().expect("create coverage cleanup deadline repository");
        let database_path = repository.path().join("coverage-cleanup-deadline.sqlite");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("coverage cleanup deadline database path is utf-8"),
        )
        .await
        .expect("create coverage cleanup deadline database");
        let backend = conn.get_database_backend();
        conn.execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode for coverage cleanup deadline regression");
        let scope = CaptureScope {
            repo_id: "coverage-cleanup-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("coverage-cleanup-deadline-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/coverage-cleanup-deadline-workspace', 'agent',
                       'active', 'coverage-cleanup-deadline-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed coverage cleanup deadline workspace lease");
        conn.execute_unprepared(
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('coverage-cleanup-deadline-session', 'claude_code',
                       'coverage-cleanup-deadline-provider', 'active', '/tmp', '{}', '{}', 0, 0, 1,
                       'coverage-cleanup-deadline-repo', '', 'coverage-cleanup-deadline-workspace', 7, 'scoped')",
        )
        .await
        .expect("seed coverage cleanup deadline session");

        let owner = "coverage-cleanup-deadline-owner";
        let turn = turn(
            "coverage-cleanup-deadline-turn",
            "coverage reservation retained under a recovery writer lock",
            Completeness::Complete,
        );
        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            "coverage-cleanup-deadline-session",
            std::slice::from_ref(&turn),
            owner,
            1,
        )
        .await
        .expect("seed owned coverage reservation before recovery lock");
        assert_eq!(reserved.reserved.len(), 1);

        let locker = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("coverage cleanup deadline database path is utf-8"),
            Duration::from_secs(1),
        )
        .await
        .expect("open coverage cleanup deadline lock holder");
        locker
            .execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode on coverage cleanup deadline lock holder");
        locker
            .execute_raw(Statement::from_string(
                backend,
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive coverage cleanup deadline lock");

        let started = Instant::now();
        let cleanup = tokio::time::timeout(
            Duration::from_secs(2),
            abandon_reserved_turn_claims_with_capture_scope_until(
                &conn,
                &scope,
                "coverage-cleanup-deadline-session",
                owner,
                "live",
                2,
                CaptureCommitDeadline::from_test_pair(
                    Instant::now() - Duration::from_millis(1),
                    chrono::Utc::now().timestamp_millis().saturating_sub(1),
                ),
            ),
        )
        .await
        .expect("coverage recovery cleanup must honor its short writer-acquisition grace");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK".to_string()))
            .await
            .expect("release exclusive coverage cleanup deadline lock");

        let error =
            cleanup.expect_err("exclusive lock must exhaust coverage cleanup recovery grace");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected coverage cleanup recovery-lock error: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "coverage cleanup must not wait for SQLite's default busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'coverage-cleanup-deadline-session'
                   AND logical_turn_key = 'coverage-cleanup-deadline-turn'
                   AND state = 'reserved_live'
                   AND owner = 'coverage-cleanup-deadline-owner'",
            )
            .await,
            1,
            "a cancelled recovery acquisition must not abandon the claim after the lock clears"
        );
    }

    #[tokio::test]
    async fn abandonment_recovery_preserves_live_primary_sqlite_deadline_at_final_authorization() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let owner = "coverage-cleanup-final-sqlite-owner";
        let turn = turn(
            "coverage-cleanup-final-sqlite-deadline",
            "coverage cleanup final authorization",
            Completeness::Complete,
        );
        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            owner,
            1,
        )
        .await
        .expect("seed owned coverage reservation before final authorization");
        assert_eq!(reserved.reserved.len(), 1);

        // The monotonic foreground budget remains live, but its original
        // SQLite authorization is already expired. Cleanup must retain that
        // stricter wall-clock ceiling rather than replace it with fresh grace.
        let error = abandon_reserved_turn_claims_with_capture_scope_until(
            &conn,
            &scope,
            session,
            owner,
            "live",
            2,
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(1), 0),
        )
        .await
        .expect_err("expired primary SQLite authorization must roll back abandonment");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected coverage cleanup final authorization error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key = 'coverage-cleanup-final-sqlite-deadline'
                   AND state = 'reserved_live'
                   AND owner = 'coverage-cleanup-final-sqlite-owner'",
            )
            .await,
            1,
            "expired final authorization must leave the owned reservation intact"
        );
    }

    #[tokio::test]
    async fn live_reservation_deadline_rolls_back_the_entire_claim_plan() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turns = [
            turn(
                "live-deadline-first",
                "first live coverage turn",
                Completeness::Complete,
            ),
            turn(
                "live-deadline-second",
                "second live coverage turn",
                Completeness::Complete,
            ),
        ];
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_millis(500),
            chrono::Utc::now().timestamp_millis().saturating_add(500),
        );
        let error = with_live_reservation_turn_delay(
            std::time::Duration::from_millis(650),
            reserve_live_turn_claims_until(
                &conn,
                &scope,
                session,
                &turns,
                "live-deadline-owner",
                1,
                deadline,
            ),
        )
        .await
        .expect_err("an elapsed live deadline must abort its all-or-nothing reservation");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected live-deadline error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key LIKE 'live-deadline-%'",
            )
            .await,
            0,
            "a deadline during a live reservation must roll back every claimed turn"
        );
    }

    #[tokio::test]
    async fn live_reservation_final_sqlite_deadline_rolls_back_claims() {
        let (conn, scope) = scoped_gate_db().await;
        let turns = [turn(
            "live-final-sqlite-deadline",
            "coverage final authorization",
            Completeness::Complete,
        )];
        // The monotonic budget is still live, but the immutable wall-clock
        // authorization is already expired. The claim DML therefore reaches
        // the final SQLite predicate and must roll back rather than publish.
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_secs(1),
            0,
        );
        let error = reserve_live_turn_claims_until(
            &conn,
            &scope,
            "claude_code__scope_expiry",
            &turns,
            "live-final-sqlite-owner",
            1,
            deadline,
        )
        .await
        .expect_err("expired final SQLite authorization must reject the claim commit");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected final authorization error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key = 'live-final-sqlite-deadline'",
            )
            .await,
            0,
            "the final authorization rejection must roll back the new claim"
        );
    }

    #[tokio::test]
    async fn export_reservation_execution_keeps_export_provenance_and_final_deadline() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let successful_turn = turn(
            "export-provenance-turn",
            "coverage reserved by the OpenCode export path",
            Completeness::Complete,
        );
        let outcome = reserve_turn_claims_for_channel_with_capture_scope_until(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&successful_turn),
            "export-provenance-owner",
            1,
            CaptureReservationExecution::export(CaptureCommitDeadline::from_test_pair(
                Instant::now() + std::time::Duration::from_secs(1),
                chrono::Utc::now().timestamp_millis().saturating_add(1_000),
            )),
        )
        .await
        .expect("export reservation should succeed within its paired deadline");
        assert_eq!(outcome.reserved.len(), 1);
        let provenance = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, source_channel FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?",
                [
                    session.into(),
                    successful_turn.logical_turn_key.clone().into(),
                ],
            ))
            .await
            .expect("read export coverage provenance")
            .expect("export reservation exists");
        assert_eq!(
            provenance
                .try_get_by::<String, _>("state")
                .expect("decode export reservation state"),
            "reserved_live"
        );
        assert_eq!(
            provenance
                .try_get_by::<String, _>("source_channel")
                .expect("decode export reservation channel"),
            "export"
        );

        let expired_turn = turn(
            "export-final-sqlite-deadline",
            "coverage must not commit after export final authorization expires",
            Completeness::Complete,
        );
        let error = reserve_turn_claims_for_channel_with_capture_scope_until(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&expired_turn),
            "export-final-sqlite-owner",
            2,
            CaptureReservationExecution::export(CaptureCommitDeadline::from_test_pair(
                Instant::now() + std::time::Duration::from_secs(1),
                0,
            )),
        )
        .await
        .expect_err("expired export SQLite authorization must roll back the claim");
        assert!(
            format!("{error:#}")
                .contains("export coverage reservation exceeded its execution deadline"),
            "unexpected export final authorization error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key = 'export-final-sqlite-deadline'",
            )
            .await,
            0,
            "an expired export final authorization must not publish a delayed claim"
        );
    }

    #[test]
    fn import_session_lifecycle_requires_state_timestamp_pair() {
        assert_eq!(
            ImportSessionLifecycleState::from_import_state("active", None)
                .expect("active import state with no terminal timestamp"),
            ImportSessionLifecycleState::Active
        );
        assert_eq!(
            ImportSessionLifecycleState::from_import_state("stopped", Some(1))
                .expect("stopped import state with terminal timestamp"),
            ImportSessionLifecycleState::Stopped
        );
        assert!(
            ImportSessionLifecycleState::from_import_state("active", Some(1)).is_err(),
            "reactivated imports must clear stale stopped_at"
        );
        assert!(
            ImportSessionLifecycleState::from_import_state("stopped", None).is_err(),
            "terminal imports must retain a stopped_at timestamp"
        );
    }

    async fn gate_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:").await.expect("mem db");
        // The migration set assumes the bootstrap schema (ai_thread etc.)
        // exists; this unit fixture only needs the capture/coverage tables,
        // so relax FK enforcement instead of replaying the full bootstrap.
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA foreign_keys = OFF".to_string(),
        ))
        .await
        .expect("pragma");
        run_builtin_migrations(&conn).await.expect("migrations");
        ConfigKv::set_with_conn(&conn, "libra.repoid", "coverage-gate-test", false)
            .await
            .expect("seed repository identity");
        // FK target for claims.
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('claude_code__s1', 'claude_code', 's1', 'active', '/tmp',
                       '{}', '{}', 0, 0, 1,
                       'coverage-gate-test', '', NULL, NULL, 'scoped')"
                .to_string(),
        ))
        .await
        .expect("seed session");
        conn
    }

    pub(crate) fn turn(key: &str, text: &str, completeness: Completeness) -> NormalizedTurn {
        NormalizedTurn {
            logical_turn_key: key.to_string(),
            ordinal: 0,
            completeness,
            started_at: None,
            ended_at: None,
            records: vec![SemanticRecord::User {
                text: text.to_string(),
            }],
        }
    }

    async fn claim_row(conn: &DatabaseConnection, key: &str) -> (String, i64, Option<i64>) {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, revision, fence_token FROM agent_coverage_claim \
                 WHERE logical_turn_key = ?",
                [key.into()],
            ))
            .await
            .expect("query")
            .expect("row");
        (
            row.try_get_by("state").unwrap(),
            row.try_get_by("revision").unwrap(),
            row.try_get_by("fence_token").ok().flatten(),
        )
    }

    async fn scalar(conn: &DatabaseConnection, sql: &str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("scalar query")
        .expect("scalar row")
        .try_get_by("n")
        .expect("scalar value")
    }

    /// Simulate the in-transaction commit of a reservation (what
    /// `LiveClaimCommitPlan::apply` does), without building objects.
    async fn commit_reserved(
        conn: &DatabaseConnection,
        session_id: &str,
        owner: &str,
        claim: &ReservedTurnClaim,
        checkpoint_id: &str,
    ) -> Result<()> {
        let txn = sea_orm::TransactionTrait::begin(conn).await?;
        let plan = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: session_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            owner: owner.to_string(),
            parent_commit: None,
            created_at: 0,
            now_ms: 1,
            claims: vec![claim.clone()],
            import_session: None,
            import_identity: None,
            capture_scope: None,
        };
        let ctx = TracesCommitCtx {
            commit_hash: format!("commit-{checkpoint_id}"),
            tree_oid: "t".to_string(),
            metadata_blob_oid: "m".to_string(),
        };
        plan.apply(&txn, &ctx).await?;
        txn.commit().await?;
        Ok(())
    }

    async fn scoped_gate_db() -> (DatabaseConnection, CaptureScope) {
        let conn = gate_db().await;
        let scope = CaptureScope {
            repo_id: "coverage-gate-test".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("coverage-gate-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/coverage-gate-workspace', 'agent',
                       'active', 'coverage-gate-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live coverage workspace");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('claude_code__scope_expiry', 'claude_code', 'scope-expiry-session',
                       'active', '/tmp', '{}', '{}', 0, 0, 1, ?, ?, ?, ?, 'scoped')",
            [
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed scoped coverage session");
        (conn, scope)
    }

    /// A workspace can expire after coverage is reserved but before object
    /// construction reaches ref-CAS. Neither the final companion writes nor
    /// abort cleanup may mutate that now-stale claim.
    #[tokio::test]
    async fn expired_workspace_scope_blocks_coverage_final_and_abandon_mutations() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turn = turn(
            "scope-expiry",
            "captured while lease was live",
            Completeness::Complete,
        );
        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            "scope-owner",
            1,
        )
        .await
        .expect("reserve while workspace lease is live");
        let claim = reserved
            .reserved
            .first()
            .expect("one reserved turn")
            .clone();

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE workspace_record SET lease_expires_at = 0 WHERE workspace_id = 'coverage-gate-workspace'"
                .to_string(),
        ))
        .await
        .expect("expire workspace lease after reservation");

        let plan = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: session.to_string(),
            checkpoint_id: "scope-expiry-checkpoint".to_string(),
            owner: "scope-owner".to_string(),
            parent_commit: None,
            created_at: 1,
            now_ms: 2,
            claims: vec![claim],
            import_session: None,
            import_identity: None,
            capture_scope: Some(scope.clone()),
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin final scope check transaction");
        let error = plan
            .apply(
                &txn,
                &TracesCommitCtx {
                    commit_hash: "scope-expiry-commit".to_string(),
                    tree_oid: "scope-expiry-tree".to_string(),
                    metadata_blob_oid: "scope-expiry-metadata".to_string(),
                },
            )
            .await
            .expect_err("expired scope must reject the final coverage transaction");
        assert!(format!("{error:#}").contains("workspace lease"));
        txn.rollback()
            .await
            .expect("roll back rejected final transaction");

        let error = abandon_reserved_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            "scope-owner",
            "live",
            2,
        )
        .await
        .expect_err("expired scope must not abandon a coverage reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'scope-expiry-checkpoint'",
            )
            .await,
            0,
            "final checkpoint catalog row must not be written"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = 'scope-expiry-checkpoint'",
            )
            .await,
            0,
            "coverage revision must not be appended"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'scope-expiry' AND state = 'reserved_live'
                   AND owner = 'scope-owner'",
            )
            .await,
            1,
            "stale cleanup must leave the original reservation untouched"
        );
    }

    /// The reservation and abandonment transactions can both mutate claims
    /// after their initial lease check. Expire the workspace from a trigger
    /// after each mutation so the final commit fence, rather than timing,
    /// proves that neither change survives.
    #[tokio::test]
    async fn post_mutation_scope_expiry_rolls_back_coverage_reserve_and_abandon() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turn = turn(
            "post-mutation-expiry",
            "coverage must stay transactional",
            Completeness::Complete,
        );

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_coverage_reserve
             AFTER INSERT ON agent_coverage_claim
             WHEN NEW.session_id = 'claude_code__scope_expiry'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'coverage-gate-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-reservation expiry trigger");
        let error = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            "post-mutation-owner",
            1,
        )
        .await
        .expect_err("expiry after claim insert must roll back reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'post-mutation-expiry'",
            )
            .await,
            0,
            "post-reservation expiry must not leave a coverage claim"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-reservation expiry trigger must roll back with the claim");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TRIGGER expire_scope_after_coverage_reserve".to_string(),
        ))
        .await
        .expect("remove post-reservation expiry trigger");

        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            "post-mutation-owner",
            1,
        )
        .await
        .expect("reserve claim while workspace lease is live");
        assert_eq!(reserved.reserved.len(), 1);
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_coverage_abandon
             AFTER UPDATE OF state ON agent_coverage_claim
             WHEN NEW.session_id = 'claude_code__scope_expiry'
               AND NEW.logical_turn_key = 'post-mutation-expiry'
               AND NEW.state = 'abandoned'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'coverage-gate-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-abandonment expiry trigger");
        let error = abandon_reserved_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            "post-mutation-owner",
            "live",
            2,
        )
        .await
        .expect_err("expiry after claim abandonment must roll back cleanup");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'post-mutation-expiry'
                   AND state = 'reserved_live' AND owner = 'post-mutation-owner'",
            )
            .await,
            1,
            "post-abandonment expiry must retain the owned reservation"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-abandonment expiry trigger must roll back with the claim");
    }

    /// ACF-08: the historical importer renews, binds and abandons its claims
    /// through these shared statements inside its own identity/attempt
    /// transaction. Each must touch only the caller's owner/fence and roll
    /// back with that transaction.
    #[tokio::test]
    async fn import_claim_transitions_share_the_callers_transaction() {
        /// `(turn, state, owner, lease_expires_at, attempt, fence)`.
        type ClaimRow = (
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            i64,
        );
        async fn import_claims(conn: &impl ConnectionTrait) -> Vec<ClaimRow> {
            conn.query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT logical_turn_key, state, owner, lease_expires_at,
                        attempt_checkpoint_id, fence_token
                 FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                 ORDER BY logical_turn_key"
                    .to_string(),
            ))
            .await
            .expect("query import claims")
            .into_iter()
            .map(|row| {
                (
                    row.try_get_by("logical_turn_key").expect("turn key"),
                    row.try_get_by("state").expect("state"),
                    row.try_get_by("owner").expect("owner"),
                    row.try_get_by("lease_expires_at").expect("lease"),
                    row.try_get_by("attempt_checkpoint_id").expect("attempt"),
                    row.try_get_by("fence_token").expect("fence"),
                )
            })
            .collect()
        }

        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let owner = "import:1:transition-owner";
        let turns = [
            turn("import-a", "first imported turn", Completeness::Complete),
            turn("import-b", "second imported turn", Completeness::Complete),
        ];
        let reserved = reserve_import_turn_claims_until(
            &conn,
            &scope,
            session,
            &turns,
            owner,
            1,
            CaptureCommitDeadline::from_test_pair(
                Instant::now() + Duration::from_secs(5),
                chrono::Utc::now().timestamp_millis().saturating_add(5_000),
            ),
        )
        .await
        .expect("reserve import claims")
        .reserved;
        assert_eq!(reserved.len(), 2);
        let before = import_claims(&conn).await;
        assert!(
            before
                .iter()
                .all(|claim| claim.1 == "reserved_import" && claim.2.as_deref() == Some(owner))
        );

        // Renewal and binding are visible only through the caller's commit.
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin rolled-back attempt transaction");
        renew_import_turn_claim_leases_with_conn(&txn, session, owner, 9_000, 2)
            .await
            .expect("renew in caller transaction");
        assert!(
            bind_import_turn_claim_attempt_with_conn(&txn, session, &reserved[0], owner, "ckpt", 2)
                .await
                .expect("bind in caller transaction")
        );
        txn.rollback().await.expect("roll back attempt transaction");
        assert_eq!(import_claims(&conn).await, before);

        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin attempt transaction");
        renew_import_turn_claim_leases_with_conn(&txn, session, "import:1:other", 7_000, 2)
            .await
            .expect("renew for a foreign owner");
        renew_import_turn_claim_leases_with_conn(&txn, session, owner, 9_000, 2)
            .await
            .expect("renew owned claims");
        let mut stale = reserved[0].clone();
        stale.fence_token += 1;
        for (claim, claim_owner) in [(&stale, owner), (&reserved[0], "import:1:other")] {
            assert!(
                !bind_import_turn_claim_attempt_with_conn(
                    &txn,
                    session,
                    claim,
                    claim_owner,
                    "ckpt",
                    2
                )
                .await
                .expect("refused bind"),
                "a foreign owner or stale fence must not bind the attempt"
            );
        }
        assert!(
            bind_import_turn_claim_attempt_with_conn(&txn, session, &reserved[0], owner, "ckpt", 2)
                .await
                .expect("bind owned claim")
        );
        txn.commit().await.expect("commit attempt transaction");
        let bound = import_claims(&conn).await;
        assert!(bound.iter().all(|claim| claim.3 == Some(9_000)));
        assert_eq!(bound[0].4.as_deref(), Some("ckpt"));
        assert_eq!(bound[1].4, None);

        // Abandonment releases exactly this owner's import reservations.
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin abandonment transaction");
        abandon_reserved_turn_claims_with_conn(&txn, session, "import:1:other", "import", 3)
            .await
            .expect("abandon for a foreign owner");
        abandon_reserved_turn_claims_with_conn(&txn, session, owner, "live", 3)
            .await
            .expect("abandon the live state for this owner");
        assert_eq!(import_claims(&txn).await, bound, "no claim may change yet");
        abandon_reserved_turn_claims_with_conn(&txn, session, owner, "import", 3)
            .await
            .expect("abandon owned import claims");
        txn.commit().await.expect("commit abandonment");
        let abandoned = import_claims(&conn).await;
        for (after, before) in abandoned.iter().zip(&bound) {
            assert_eq!(
                (after.1.as_str(), &after.2, after.3, &after.4, after.5),
                ("abandoned", &None, None, &None, before.5 + 1),
                "abandonment must release the lease and advance the fence"
            );
        }
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin late bind transaction");
        assert!(
            !bind_import_turn_claim_attempt_with_conn(
                &txn,
                session,
                &reserved[1],
                owner,
                "late",
                4
            )
            .await
            .expect("late bind"),
            "an abandoned reservation must reject its former owner"
        );
        assert!(
            abandon_reserved_turn_claims_with_conn(&txn, session, owner, "unknown", 4)
                .await
                .is_err(),
            "an unknown source channel must fail closed"
        );
        txn.rollback().await.expect("roll back late bind");
    }

    /// crash_after_claim_before_objects_recovers: a writer that reserved a
    /// claim and died before building objects must not block the turn — after
    /// the lease expires the next writer takes over and commits normally.
    #[tokio::test]
    async fn crash_after_claim_before_objects_recovers() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t = turn("u1", "hi", Completeness::Complete);

        // Crashed writer: reserved, never committed.
        let dead = reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "dead", 0)
            .await
            .expect("reserve");
        assert_eq!(dead.reserved.len(), 1);

        // Before lease expiry the turn is in-flight (no takeover).
        let blocked =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "next", 1_000)
                .await
                .expect("reserve while leased");
        assert_eq!(blocked.skipped_inflight, 1);
        assert!(blocked.reserved.is_empty());

        // After expiry: takeover + normal commit → the turn recovers.
        let recovered =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "next", 100_000)
                .await
                .expect("takeover");
        assert_eq!(recovered.reserved.len(), 1);
        commit_reserved(
            &conn,
            session,
            "next",
            &recovered.reserved[0],
            "cp-recovered",
        )
        .await
        .expect("commit after takeover");
        let (state, revision, _) = claim_row(&conn, "u1").await;
        assert_eq!(state, "catalog_committed");
        assert_eq!(revision, 1);
    }

    /// ADR-ACF-09a: claims handed to a durable pending terminal artifact are
    /// never taken over once the ordinary 60 s lease would have expired —
    /// not by a live redelivery/resumed writer and not by an import — yet the
    /// artifact's own owner/fence still commits them on replay. The control
    /// turn proves the same clock does take over an ordinary expired claim.
    #[tokio::test]
    async fn artifact_retained_claims_survive_lease_expiry_until_replay_commit() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let retained_turn = turn("retained", "artifact turn", Completeness::Complete);
        let control_turn = turn("control", "ordinary turn", Completeness::Complete);
        let artifact = reserve_live_turn_claims(
            &conn,
            session,
            &[retained_turn.clone(), control_turn.clone()],
            "artifact-owner",
            0,
        )
        .await
        .expect("reserve artifact turns");
        assert_eq!(artifact.reserved.len(), 2);
        let retained_claim = artifact
            .reserved
            .iter()
            .find(|claim| claim.logical_turn_key == "retained")
            .expect("retained claim reserved")
            .clone();
        retain_reserved_live_claims_for_artifact_with_conn(
            &conn,
            session,
            "artifact-owner",
            std::slice::from_ref(&retained_claim),
        )
        .await
        .expect("hand the verified claim to the artifact");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'retained' AND state = 'reserved_live'
                   AND owner = 'artifact-owner' AND fence_token = 1
                   AND lease_expires_at = 9223372036854775807",
            )
            .await,
            1,
            "retention must mark only the verified claim"
        );
        let foreign = retain_reserved_live_claims_for_artifact_with_conn(
            &conn,
            session,
            "foreign-owner",
            std::slice::from_ref(&retained_claim),
        )
        .await
        .expect_err("a foreign owner cannot retain another writer's claim");
        assert!(format!("{foreign:#}").contains("coverage ownership changed"));

        // Far past the ordinary lease: the redelivery/resumed live writer and
        // an import both observe an in-flight retained turn, never a takeover.
        let live = reserve_live_turn_claims(
            &conn,
            session,
            &[retained_turn.clone(), control_turn.clone()],
            "redelivery-owner",
            10 * LIVE_LEASE_MS,
        )
        .await
        .expect("live reservation after the ordinary lease");
        assert_eq!(live.skipped_inflight, 1);
        assert_eq!(live.skipped_retained, 1);
        assert_eq!(live.reserved.len(), 1);
        assert_eq!(live.reserved[0].logical_turn_key, "control");
        let import = reserve_turn_claims_for_channel(
            &conn,
            session,
            std::slice::from_ref(&retained_turn),
            "import-owner",
            20 * LIVE_LEASE_MS,
            "import",
        )
        .await
        .expect("import reservation after the ordinary lease");
        assert!(import.reserved.is_empty());
        assert_eq!(import.skipped_inflight, 1);
        assert_eq!(import.skipped_retained, 1);
        assert!(import.is_inflight_only_skip());
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'retained' AND state = 'reserved_live'
                   AND owner = 'artifact-owner' AND fence_token = 1 AND revision = 0",
            )
            .await,
            1,
            "no later writer may advance the artifact's fence"
        );

        commit_reserved(
            &conn,
            session,
            "artifact-owner",
            &retained_claim,
            "cp-artifact-replay",
        )
        .await
        .expect("artifact replay commits under its original owner/fence");
        let (state, revision, fence) = claim_row(&conn, "retained").await;
        assert_eq!(state, "catalog_committed");
        assert_eq!(revision, 1);
        assert_eq!(fence, Some(1));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'retained' AND lease_expires_at IS NULL",
            )
            .await,
            1,
            "the replay commit releases the retention sentinel"
        );
    }

    #[tokio::test]
    async fn tombstone_blocks_reservation_and_fences_reserved_commit() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t = turn("erase-turn", "hi", Completeness::Complete);
        let reserved =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "stale", 0)
                .await
                .expect("initial reservation");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_import_tombstone (
                tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at
             ) VALUES ('t-coverage', 'claude_code', 's1', 'claude_code__s1', 1)"
                .to_string(),
        ))
        .await
        .expect("seed tombstone");

        let error = commit_reserved(&conn, session, "stale", &reserved.reserved[0], "cp-erased")
            .await
            .expect_err("final transaction must observe tombstone");
        assert!(error.to_string().contains("tombstoned"));
        let error = reserve_live_turn_claims(&conn, session, &[t], "new", 2)
            .await
            .expect_err("new reservation must observe tombstone");
        assert!(error.to_string().contains("erased"));
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'cp-erased'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 0);
    }

    /// live_takeover_increments_fence_and_blocks_(import|stale)_ref_cas:
    /// an expired reservation is taken over with a HIGHER fence; the stale
    /// holder's commit then fails its fence check and must roll back.
    #[tokio::test]
    async fn live_takeover_increments_fence_and_blocks_stale_commit() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t = turn("u1", "hi", Completeness::Complete);

        // Stale writer reserves at now=0 (lease expires at 60_000).
        let stale =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "stale-owner", 0)
                .await
                .expect("reserve");
        assert_eq!(stale.reserved.len(), 1);
        let stale_claim = stale.reserved[0].clone();
        assert_eq!(stale_claim.fence_token, 1);

        // Lease expired: a new writer takes over with fence 2.
        let fresh = reserve_live_turn_claims(
            &conn,
            session,
            std::slice::from_ref(&t),
            "fresh-owner",
            100_000,
        )
        .await
        .expect("takeover");
        assert_eq!(fresh.reserved.len(), 1);
        assert_eq!(fresh.reserved[0].fence_token, 2);

        // The stale holder's commit must fail closed (fence mismatch).
        let err = commit_reserved(&conn, session, "stale-owner", &stale_claim, "cp-stale")
            .await
            .expect_err("stale fence must be rejected");
        assert!(err.to_string().contains("fenced out"), "got: {err:#}");
        let (state, revision, fence) = claim_row(&conn, "u1").await;
        assert_eq!(state, "reserved_live");
        assert_eq!(revision, 0, "stale commit must not advance the claim");
        assert_eq!(fence, Some(2));
        // coverage_revision_atomic_current_pointer: the failed transaction
        // must leave NOTHING behind — no catalog row, no revision row; the
        // claim pointer, revision history and catalog stay consistent
        // together.
        let count = |sql: &'static str| {
            let conn = conn.clone();
            async move {
                let row = conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        sql.to_string(),
                    ))
                    .await
                    .expect("count query")
                    .expect("count row");
                let n: i64 = row.try_get_by("n").expect("count");
                n
            }
        };
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'cp-stale'")
                .await,
            0,
            "rolled-back transaction must not leave a catalog row"
        );
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM agent_coverage_revision").await,
            0,
            "rolled-back transaction must not leave a revision row"
        );

        // The fresh holder commits fine.
        commit_reserved(
            &conn,
            session,
            "fresh-owner",
            &fresh.reserved[0],
            "cp-fresh",
        )
        .await
        .expect("fresh commit");
        let (state, revision, _) = claim_row(&conn, "u1").await;
        assert_eq!(state, "catalog_committed");
        assert_eq!(revision, 1);
    }

    /// ordinal_source_reorder_conflicts: a committed complete turn whose
    /// content changes (e.g. source reorder under an ordinal key) parks the
    /// claim as `conflicted` — never silently re-covered.
    #[tokio::test]
    async fn committed_complete_content_change_conflicts() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let original = turn("ordinal:0", "first", Completeness::Complete);
        let reserved = reserve_live_turn_claims(&conn, session, &[original], "w1", 0)
            .await
            .expect("reserve");
        commit_reserved(&conn, session, "w1", &reserved.reserved[0], "cp1")
            .await
            .expect("commit");

        // Reordered/rewritten source: same logical key, different complete
        // content.
        let secret_key = "AKIAIOSFODNN7EXAMPLE";
        let secret_value = format!("sk-ant-{}", "a".repeat(40));
        let mut secret_input = std::collections::BTreeMap::new();
        secret_input.insert(
            secret_key.to_string(),
            crate::internal::ai::observed_agents::CanonValue::Str(secret_value.clone()),
        );
        let reordered = NormalizedTurn {
            logical_turn_key: "ordinal:0".to_string(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: vec![SemanticRecord::ToolCall {
                call_id: Some("call-2".to_string()),
                input: crate::internal::ai::observed_agents::CanonValue::Object(secret_input),
                name: "inspect".to_string(),
            }],
        };
        let outcome = reserve_live_turn_claims(
            &conn,
            session,
            std::slice::from_ref(&reordered),
            "w2",
            1_000,
        )
        .await
        .expect("reserve conflict");
        assert!(outcome.reserved.is_empty());
        assert_eq!(outcome.conflicted, 1);
        let (state, revision, _) = claim_row(&conn, "ordinal:0").await;
        assert_eq!(state, "conflicted");
        assert_eq!(revision, 1, "committed revision is preserved for doctor");
        let conflict = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT incumbent_revision, incumbent_digest, incoming_digest,
                        incoming_source_channel, incoming_canonical_json,
                        incoming_redaction_report_json
                 FROM agent_coverage_conflict"
                    .to_string(),
            ))
            .await
            .expect("query conflict evidence")
            .expect("conflict evidence row");
        assert_eq!(
            conflict.try_get_by::<i64, _>("incumbent_revision").unwrap(),
            1
        );
        let mut expected = reordered.clone();
        let expected_report = redact_turns_with_report(std::slice::from_mut(&mut expected));
        let incoming_digest = conflict.try_get_by::<String, _>("incoming_digest").unwrap();
        assert_eq!(incoming_digest, expected.digest_hex());
        assert_eq!(
            conflict
                .try_get_by::<String, _>("incoming_source_channel")
                .unwrap(),
            "live"
        );
        let canonical = conflict
            .try_get_by::<String, _>("incoming_canonical_json")
            .unwrap();
        assert!(!canonical.contains(secret_key));
        assert!(!canonical.contains(&secret_value));
        assert!(canonical.contains("redacted-key-sha256-"));
        assert!(canonical.contains("<REDACTED:anthropic-api-key>"));
        assert_eq!(
            canonical.as_bytes(),
            canonical_turn_bytes(&expected.records),
            "stored candidate must be the exact sanitized payload hashed by the digest"
        );
        let stored_report: serde_json::Value = serde_json::from_str(
            &conflict
                .try_get_by::<String, _>("incoming_redaction_report_json")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            stored_report,
            serde_json::to_value(&expected_report).unwrap()
        );
        assert!(expected_report.bytes_redacted > 0);
        assert!(
            expected_report
                .matches
                .iter()
                .any(|entry| entry.rule_id == "aws-access-key-id")
        );
        assert!(
            expected_report
                .matches
                .iter()
                .any(|entry| entry.rule_id == "anthropic-api-key")
        );
    }

    /// shared_live_snapshot_upgrade_keeps_other_turns_visible (claim level):
    /// upgrading ONE turn of a multi-turn snapshot leaves the other turns'
    /// committed claims and revisions untouched.
    #[tokio::test]
    async fn upgrading_one_turn_leaves_other_claims_untouched() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t1 = turn("u1", "one", Completeness::Complete);
        let t2 = turn("u2", "two", Completeness::Incomplete);

        let first = reserve_live_turn_claims(&conn, session, &[t1.clone(), t2], "w1", 0)
            .await
            .expect("reserve both");
        assert_eq!(first.reserved.len(), 2);
        for claim in &first.reserved {
            commit_reserved(&conn, session, "w1", claim, "cp1")
                .await
                .expect("commit");
        }

        // Second snapshot: t1 unchanged (skip), t2 now complete (upgrade).
        let t2_complete = turn("u2", "two done", Completeness::Complete);
        let second = reserve_live_turn_claims(&conn, session, &[t1, t2_complete], "w2", 1_000)
            .await
            .expect("reserve upgrade");
        assert_eq!(second.skipped_covered, 1, "t1 already covered");
        assert_eq!(second.reserved.len(), 1, "only t2 upgrades");
        commit_reserved(&conn, session, "w2", &second.reserved[0], "cp2")
            .await
            .expect("commit upgrade");

        let (s1, r1, _) = claim_row(&conn, "u1").await;
        assert_eq!((s1.as_str(), r1), ("catalog_committed", 1), "t1 untouched");
        let (s2, r2, _) = claim_row(&conn, "u2").await;
        assert_eq!((s2.as_str(), r2), ("catalog_committed", 2), "t2 advanced");
        // Both checkpoints remain in the catalog — no checkpoint-level
        // supersede (ADR-DR-16).
        let rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint".to_string(),
            ))
            .await
            .expect("checkpoints");
        assert_eq!(rows.len(), 2);
    }
}
