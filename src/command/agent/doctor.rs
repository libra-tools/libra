//! `libra agent doctor [--repair]` — capture-store diagnostics and repair.
//!
//! Detection surfaces hook installation state, stuck active sessions, orphan
//! checkpoints, and — AG-20 (plan.md Task A5, `agent.md` doctor repair
//! matrix) — checkpoint-store inconsistency classes plus writer-marker
//! recovery state:
//!
//! 1. **`stale_catalog_row` / `missing_objects`** — an `agent_checkpoint`
//!    row whose `traces_commit` / `tree_oid` / `metadata_blob_oid` disagree
//!    with the object store. When the checkpoint is still reachable from
//!    `refs/libra/traces`, the row is rebuilt from the ref (idempotent
//!    UPDATE). When objects are genuinely missing, the finding is reported
//!    as `missing_objects` and requires manual action — doctor never
//!    destroys or fabricates data. For E4 (non-legacy) checkpoints the
//!    existence check covers the FULL six-entry tree, not just the row
//!    columns: `manifest.json`, `events/lifecycle.jsonl`,
//!    `transcript/<agent_kind>.jsonl` (including E5 `.jsonl.%03d` chunks),
//!    `redaction_report.json`, `content_hash.txt`, the intermediate trees,
//!    plus every blob the manifest declares — so a checkpoint whose
//!    sidecar is gone is never reported healthy while `show`/export cannot
//!    reconstruct it. A missing `manifest.json` blob is itself a
//!    `missing_objects` finding, and the tree-entry enumeration remains
//!    the primary probe so one missing manifest never hides other missing
//!    sidecars.
//! 2. **`missing_catalog_row`** — a checkpoint reachable from
//!    `refs/libra/traces` with no `agent_checkpoint` row (crash window B:
//!    ref CAS succeeded, catalog INSERT did not). Repair re-INSERTs the row
//!    via the same probe-first idempotent path the writer uses
//!    (`capture::checkpoint::insert_agent_checkpoint_row_idempotent`), reconstructing
//!    the columns from the commit's `metadata.json` (both the AG-20 v2 and
//!    the legacy v1 metadata shapes parse) plus the `Libra-*` commit
//!    trailers. Checkpoints named by a LIVE traces in-flight marker are
//!    writers mid-flight, not inconsistencies, and are skipped.
//! 3. **`missing_object_index`** — a checkpoint object with no
//!    `object_index` row, i.e. invisible to `libra cloud sync`. Covers the
//!    checkpoint's full writer-enqueued set: the traces commit plus every
//!    E4 object the class-1 sweep verified, with the writer's o_type tags
//!    (trees as `tree`, transcript blobs/chunks as `agent_transcript`,
//!    JSON/text sidecars as `blob` — mirroring the
//!    `history.rs::append_checkpoint_commit` / `splice_checkpoint_tree`
//!    enqueue calls). `o_size` is never taken from manifest `byte_len`:
//!    doctor streams a descriptor-pinned loose object through its
//!    content-addressed hash under the transcript cap and retains no payload
//!    bytes before writing the verified size. Repair inserts rows directly
//!    (idempotent existence-checked INSERT mirroring
//!    `client_storage::update_object_index_once` semantics — doctor is a
//!    foreground command, so it does not go through the background queue).
//!    Only rows with an UNRECOVERABLE class-1 finding fall back to class-1
//!    reporting; an auto-repairable stale row still gets its (ref-side)
//!    class-3 check and repair in the same `--repair` run.
//! 4. **`expired_inflight_marker` / `invalid_inflight_marker`** — valid
//!    expired writer markers are drained through the serialized all-ref
//!    reachability cleanup; malformed rows are reported manual-required
//!    because their candidate ownership cannot be decoded safely.
//!
//! **Legacy-v1 exemption**: checkpoints whose tree lacks `manifest.json`
//! (pre-AG-20 layout, `metadata.json` + `transcript/<provider>` only — see
//! `tests/fixtures/agent_checkpoints/v1_claude_code/`) are classified
//! `legacy-v1`, counted in `legacy_v1_checkpoints`, and NEVER included in
//! checkpoint-object repair classes or touched by `--repair`.
//!
//! **Orphan rule fidelity** (`agent.md` write-sequence section):
//! session-without-checkpoint is a LEGAL intermediate state and is never
//! flagged; only checkpoint-without-session counts as an orphan.
//!
//! **Gemini**: existing `agent_session` / `agent_checkpoint` rows with
//! `agent_kind = 'gemini'` are legal read-only data and are never flagged.
//! Leftover gemini hook *configuration*, however, gets an actionable hint
//! pointing at the uninstall-only channel (`libra agent remove gemini`).
//!
//! **Observability** (`agent.md` §6): with `--repair`, one
//! `agent.doctor.repair` span is emitted per repair attempt (including
//! attempts that end `manual_required`) carrying `inconsistency_type`,
//! `repaired`, `manual_required`. Raw transcript bytes never reach the
//! span sink — doctor is metadata-first and never materializes or renders
//! transcript blobs.
//! Detection-only runs emit no repair spans (nothing was attempted).
//!
//! Without `--repair` the command is strictly read-only; findings report
//! what `--repair` would do (`repaired` stays `false`, `manual_required`
//! is already accurate). All repairs are idempotent: a second run finds a
//! consistent store and does nothing.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use git_internal::{
    hash::{ObjectHash, set_hash_kind},
    internal::object::{
        ObjectTrait,
        commit::Commit,
        tree::{Tree, TreeItemMode},
    },
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    QueryFilter, Statement, TransactionTrait,
};
use serde::Serialize;

use super::DoctorArgs;
use crate::{
    internal::{
        ai::{
            capture::{
                catalog::{
                    CaptureCatalogFinalizerRecovery, CaptureCatalogFinalizerRecoveryResult,
                    CaptureCatalogStore,
                },
                checkpoint::{
                    AgentCheckpointRow, SubagentCheckpointRow,
                    insert_agent_checkpoint_row_idempotent,
                    insert_subagent_checkpoint_row_idempotent,
                },
                pending,
                recovery::{self, ArtifactRecoveryOutcome},
            },
            capture_scope::{CaptureCommitDeadline, CaptureScope},
            history::HistoryManager,
            hooks::providers::{claude_provider, gemini_provider},
            observed_agents::{AgentStability, PREVIEW_SPECS, STABLE_PROMOTED_SPECS},
            traces,
        },
        config::ConfigKv,
        db::get_db_conn_instance_for_path,
        model::reference::{self, ConfigKind},
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// Upper bound on the first-parent traces walk — prevents a corrupt
/// (cyclic) chain from hanging doctor. Hitting the cap truncates the walk
/// with a note; truncation only ever *under*-detects (fail-safe direction).
const MAX_TRACES_WALK_COMMITS: usize = 100_000;
const MAX_IMPORT_INDEX_REPAIR_CHECKPOINTS: usize = 4_096;
/// Doctor never needs an unbounded payload. Trees, commits, manifests, and
/// findings sidecars are control-plane objects; transcript bodies are never
/// read here. A corrupt loose object therefore cannot turn a diagnostic into
/// an unbounded decompression/allocation path.
const DOCTOR_LOCAL_OBJECT_READ_CAP_BYTES: u64 = 4 * 1024 * 1024;
/// Object-index repair must prove the actual size it writes. It streams a
/// held loose-object descriptor through the repository hash without retaining
/// payload bytes; the capture source cap bounds that work even for a
/// transcript blob.
const DOCTOR_STREAMING_OBJECT_VALIDATION_CAP_BYTES: u64 =
    crate::internal::ai::observed_agents::TRANSCRIPT_READ_HARD_CAP_BYTES;
/// Bound the aggregate decompressed object content that one doctor/import
/// index-repair pass can validate. Per-object caps alone would still permit a
/// damaged checkpoint store to make a foreground diagnostic stream many
/// thousands of individually valid transcript objects. Exhausting this
/// budget fails closed: the affected checkpoint requires manual repair and
/// receives no object-index writes.
const DOCTOR_OBJECT_INDEX_VALIDATION_TOTAL_CAP_BYTES: u64 = 128 * 1024 * 1024;
/// Review/investigate sidecars are untrusted local input. Keeping their cap
/// equal to doctor object reads bounds both a hostile manifest and a findings
/// rewrite candidate without silently reading external data through a link.
const DOCTOR_AGENT_RUN_SIDECAR_READ_CAP_BYTES: u64 = 4 * 1024 * 1024;
/// A damaged or attacker-controlled agent-runs directory must not turn a
/// foreground diagnostic into an unbounded directory walk. This comfortably
/// exceeds the normal number of retained review/investigate runs while making
/// an intentionally huge store fail closed for repair purposes.
const DOCTOR_AGENT_RUN_ENTRY_CAP: usize = 4_096;
/// A damaged E4 tree or manifest can contain arbitrarily many legal-looking
/// entries within its byte cap. Bound both dimensions before cross-checking so
/// doctor remains predictable and refuses to auto-repair an incomplete view.
const DOCTOR_E4_TREE_ENTRY_CAP: usize = 4_096;
const DOCTOR_E4_MANIFEST_DECLARATION_CAP: usize = 4_096;
const DOCTOR_E4_OBJECT_CAP: usize = 4_096;

// `cfg!(debug_assertions)` is true for ordinary developer builds, so test
// rendezvous state must be compiled only into unit-test binaries.
#[cfg(test)]
mod test_support {
    use std::sync::{Mutex, OnceLock, mpsc};

    use anyhow::{Result, anyhow};

    pub(super) struct TestPause {
        pub(super) reached: mpsc::Sender<()>,
        pub(super) resume: mpsc::Receiver<()>,
    }

    static PAUSE: OnceLock<Mutex<Option<TestPause>>> = OnceLock::new();

    fn pause() -> &'static Mutex<Option<TestPause>> {
        PAUSE.get_or_init(|| Mutex::new(None))
    }

    fn lock_pause() -> std::sync::MutexGuard<'static, Option<TestPause>> {
        pause()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) struct PauseReset(Option<TestPause>);

    pub(super) fn install(test_pause: TestPause) -> PauseReset {
        PauseReset((*lock_pause()).replace(test_pause))
    }

    impl Drop for PauseReset {
        fn drop(&mut self) {
            let _ = std::mem::replace(&mut *lock_pause(), self.0.take());
        }
    }

    pub(super) fn wait_for_pause() -> Result<()> {
        let Some(pause) = lock_pause().take() else {
            return Ok(());
        };
        let _ = pause.reached.send(());
        pause
            .resume
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| anyhow!("test import index-repair pause timed out"))?;
        Ok(())
    }
}

#[cfg(test)]
fn import_index_repair_test_pause_after_lock() -> anyhow::Result<()> {
    test_support::wait_for_pause()
}

#[cfg(not(test))]
fn import_index_repair_test_pause_after_lock() -> anyhow::Result<()> {
    Ok(())
}

/// Stable `inconsistency_type` values (also the span field values).
const CLASS_MISSING_OBJECTS: &str = "missing_objects";
const CLASS_STALE_CATALOG_ROW: &str = "stale_catalog_row";
const CLASS_MISSING_CATALOG_ROW: &str = "missing_catalog_row";
const CLASS_MISSING_OBJECT_INDEX: &str = "missing_object_index";
const CLASS_INVALID_INFLIGHT_MARKER: &str = "invalid_inflight_marker";
const CLASS_EXPIRED_INFLIGHT_MARKER: &str = "expired_inflight_marker";
const CLASS_CONFLICTED_COVERAGE_CLAIM: &str = "conflicted_coverage_claim";
const CLASS_UNRESOLVED_SUBAGENT_LINK: &str = "unresolved_subagent_link";
const CLASS_INCONSISTENT_SUBAGENT_CONTENT: &str = "inconsistent_subagent_content";
/// A0-06: a run manifest's `findings_oid` points at a blob missing from the
/// object store.
const CLASS_MISSING_FINDINGS_OBJECT: &str = "missing_findings_object";
/// A0-06: a run's findings blob exists but has no (or a drifted)
/// `object_index` row (invisible to cloud sync / retention).
const CLASS_MISSING_FINDINGS_OBJECT_INDEX: &str = "missing_findings_object_index";

#[derive(Debug, Serialize)]
struct ProviderHookStatus {
    name: &'static str,
    /// Adapter stability tier — `Stable` adapters carry a real
    /// `HookProvider` and report installation status; `Preview` ones
    /// (Phase 3.1) surface as "not yet installable".
    tier: AgentStability,
    installed: Option<bool>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    schema_present: bool,
    active_sessions: i64,
    stopped_sessions: i64,
    orphan_checkpoints: i64,
    provider_hooks: Vec<ProviderHookStatus>,
    /// AG-20: leftover gemini hook configuration detected (uninstall-only
    /// channel — see the `libra agent remove gemini` hint). Captured gemini
    /// session/checkpoint *rows* are legal read-only data, never flagged.
    gemini_hooks_remnant: bool,
    /// AG-20 checkpoint-store/marker scan (detection + repair).
    checkpoint_store: CheckpointStoreReport,
    /// A0-06 review/investigate findings-object scan (detection + repair).
    findings_store: FindingsStoreReport,
    /// RC-31: frozen Code-era residue (read-only existence check).
    legacy_code_residue: LegacyCodeResidue,
}

/// plan-20260920 RC-31: frozen Code-era residue. Read-only — this diagnostic
/// never unlinks files, rewrites objects or touches `ai_*` /
/// `agent_usage_stats`; RC-11 stopped writing this state and DEFER-RC-02 owns
/// any future cleanup.
#[derive(Debug, Serialize)]
struct LegacyCodeResidue {
    /// Repository-relative paths that still exist (never absolute).
    paths: Vec<String>,
    /// The frozen intent ref still exists in the ref store.
    #[serde(skip_serializing_if = "Option::is_none")]
    intent_ref: Option<String>,
    note: &'static str,
}

const LEGACY_CODE_RESIDUE_NOTE: &str =
    "frozen Code-era residue, kept read-only; see plan-20260920 ADR-RC-04";

#[derive(Debug, Serialize)]
struct FindingsStoreReport {
    scanned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// Runs whose manifest carries a non-null `findings_oid`.
    runs_with_findings: usize,
    repair_applied: bool,
    repaired: usize,
    manual_required: usize,
    findings: Vec<FindingsObjectFinding>,
}

#[derive(Debug, Serialize)]
struct FindingsObjectFinding {
    /// `missing_findings_object` or `missing_findings_object_index`.
    inconsistency_type: String,
    /// Review/investigate run id (the `agent-runs/<run_id>` name), emitted
    /// only after `is_valid_run_id` so `review show <run_id>` can act on it.
    run_id: String,
    /// Diagnosis — OIDs/reasons only, never findings content.
    detail: String,
    repaired: bool,
    manual_required: bool,
}

#[derive(Debug, Serialize)]
struct CheckpointStoreReport {
    /// Whether the checkpoint-store scan ran at all (requires the agent
    /// schema and a resolvable `.libra` directory).
    scanned: bool,
    /// Degradation notes (walk truncation, unreadable trees, …). The scan
    /// fails soft: unreadable pieces under-detect rather than erroring out.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    catalog_rows: i64,
    ref_reachable_checkpoints: usize,
    /// Pre-AG-20 layout checkpoints (no `manifest.json`). Exempt from the
    /// checkpoint-object inconsistency classes and from `--repair` by contract.
    legacy_v1_checkpoints: usize,
    /// LIVE traces-writer in-flight markers (window A/B guards). Commits
    /// they name are writers mid-flight and are excluded from class 2.
    live_inflight_markers: usize,
    /// Whether `--repair` was requested for this run.
    repair_applied: bool,
    repaired: usize,
    manual_required: usize,
    findings: Vec<CheckpointFinding>,
}

#[derive(Debug, Serialize)]
struct CheckpointFinding {
    /// Stable checkpoint-store or writer-marker inconsistency class.
    inconsistency_type: String,
    checkpoint_id: String,
    /// Human-readable diagnosis — OIDs and reasons only, never transcript
    /// or metadata content.
    detail: String,
    repaired: bool,
    manual_required: bool,
}

/// What `--repair` would do for one finding. Built at detection time so
/// `manual_required` is accurate even without `--repair`.
#[derive(Debug)]
enum RepairPlan {
    /// A valid expired writer marker: run the serialized root-fenced
    /// ownership retirement. Empty markers are simply cleared.
    RepairExpiredInflightMarker {
        session_id: String,
        attempt_id: String,
        observed_at_ms: i64,
    },
    /// Class 2: probe-first idempotent catalog INSERT.
    InsertCatalogRow {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
    /// Class 2, subagent scope (A0-02): probe-first idempotent catalog
    /// INSERT rebuilding a `scope='subagent'` row with its linkage columns.
    InsertSubagentCatalogRow {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        parent_checkpoint_id: Option<String>,
        subagent_session_id: Option<String>,
        tool_use_id: Option<String>,
        description: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
    /// Class 1 (recoverable): rebuild the row's OID columns from the ref.
    UpdateCatalogRow {
        checkpoint_id: String,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
    },
    /// Class 3: idempotent `object_index` inserts, `(oid, o_type, o_size)`.
    InsertObjectIndex {
        entries: Vec<(String, String, i64)>,
        /// Rows that exist but drifted from writer semantics
        /// (`(o_id, expected_o_type, expected_o_size)`); repaired via
        /// in-place UPDATE instead of insert.
        updates: Vec<(String, String, i64)>,
    },
    /// A terminal receipt survived a crash after its checkpoint became
    /// durable. The catalog revalidates its original marker/source fence
    /// before publishing the deferred terminal state.
    RecoverPendingFinalizer {
        recovery: CaptureCatalogFinalizerRecovery,
        observed_at_ms: i64,
    },
    /// A pending finalizer has exhausted its persisted replay/window budget
    /// without a durable checkpoint, so doctor may only quarantine it.
    QuarantineExhaustedPendingFinalizer {
        recovery: CaptureCatalogFinalizerRecovery,
        observed_at_ms: i64,
    },
    /// A superseded or quarantined session must leave its artifact header out
    /// of the pending replay window so it cannot block unrelated candidates.
    QuarantineStalePendingArtifact {
        recovery: CaptureCatalogFinalizerRecovery,
    },
    /// Replay one authenticated artifact through the local checkpoint writer
    /// while the original finalizer budget still permits another attempt.
    ReplayPendingArtifact {
        recovery: CaptureCatalogFinalizerRecovery,
        observed_at_ms: i64,
    },
    /// No automatic action is safe; a human must restore objects/rows.
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinalizerRepairKind {
    Manual,
    CompleteDurable,
    QuarantineExhausted,
    QuarantineStaleArtifact,
    ReplayArtifact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactReplayDisposition {
    Completed,
    PermanentFailure,
    Deferred,
    RetryLater,
    Refused,
}

/// Diagnosis never grants source access. In particular, a newer session
/// revision wins even when the old deterministic checkpoint is durable.
fn diagnose_pending_finalizer(
    manual_only: bool,
    durable: bool,
    budget_exhausted: bool,
    artifact_present: bool,
    artifact_pending: bool,
    artifact_manual_attempted: bool,
) -> (&'static str, FinalizerRepairKind) {
    if manual_only {
        if artifact_present && artifact_pending {
            (
                "terminal finalizer evidence is superseded or session-quarantined; doctor will retain its header in quarantine so it cannot block eligible recovery, without overwriting the current session",
                FinalizerRepairKind::QuarantineStaleArtifact,
            )
        } else {
            (
                "terminal finalizer evidence is superseded or session-quarantined; manual recovery is required and doctor will not overwrite the current session",
                FinalizerRepairKind::Manual,
            )
        }
    } else if durable {
        (
            "terminal finalizer remains pending after its checkpoint became durable; `libra agent doctor --repair` will revalidate the persisted marker/source fence and complete the deferred terminal receipt",
            FinalizerRepairKind::CompleteDurable,
        )
    } else if artifact_present && budget_exhausted && artifact_manual_attempted {
        (
            "terminal finalizer exhausted its original replay budget and its one audited manual attempt is already consumed; doctor will not charge another attempt",
            FinalizerRepairKind::Manual,
        )
    } else if artifact_present && budget_exhausted {
        (
            "terminal finalizer exhausted its original replay budget; repair retains the artifact header in quarantine without changing the original receipt, and authenticated manual recovery is required",
            FinalizerRepairKind::QuarantineExhausted,
        )
    } else if artifact_present && !artifact_pending {
        (
            "authenticated capture evidence is already parked in quarantine; automatic replay is disabled until the original retry budget expires and an explicit repair is requested",
            FinalizerRepairKind::Manual,
        )
    } else if artifact_present {
        (
            "authenticated pending capture artifact is eligible for one bounded local replay; `libra agent doctor --repair` will revalidate catalog and coverage fences before checkpoint publication",
            FinalizerRepairKind::ReplayArtifact,
        )
    } else if budget_exhausted {
        (
            "terminal finalizer exhausted its persisted replay budget without a ref-reachable durable checkpoint; `libra agent doctor --repair` will quarantine it for manual recovery",
            FinalizerRepairKind::QuarantineExhausted,
        )
    } else {
        // Every artifact-present case returned above.
        (
            "terminal finalizer has no durable recovery artifact (pending_source); doctor cannot reopen provider sources, and replay through the provider or explicit manual recovery is required",
            FinalizerRepairKind::Manual,
        )
    }
}

/// The ownership mode of a catalog row repaired by doctor.
///
/// Old captures intentionally remain repairable without a workspace fence:
/// they predate durable ownership and are explicitly marked
/// `legacy_unknown`. Every new scoped row instead carries a workspace fence
/// that doctor must honor even though the repair is operator initiated.
#[derive(Debug)]
enum DoctorCatalogRepairScope {
    LegacyUnscoped,
    Scoped(CaptureScope),
}

/// Repository-relative paths of the frozen Code-era residue that still exist
/// under the storage root. Only existence is reported (never absolute paths,
/// never file contents).
fn legacy_code_residue_paths(storage: &Path) -> Vec<String> {
    [
        (
            ".libra/sessions/code",
            storage.join("sessions").join("code"),
        ),
        (".libra/code", storage.join("code")),
    ]
    .into_iter()
    .filter(|(_, path)| path.exists())
    .map(|(relative, _)| relative.to_string())
    .collect()
}

/// RC-31: read-only existence check for the frozen Code-era residue:
/// `.libra/sessions/code/`, `.libra/code/` and the `libra/intent` ref. Only
/// paths and a ref name are reported — never transcript content, and never an
/// absolute path outside the repository.
async fn scan_legacy_code_residue(conn: &DatabaseConnection) -> CliResult<LegacyCodeResidue> {
    let paths = util::try_get_storage_path(None)
        .map(|storage| legacy_code_residue_paths(&storage))
        .unwrap_or_default();
    let intent_ref = reference::Entity::find()
        .filter(reference::Column::Name.eq(crate::internal::ai::history::AI_REF))
        .filter(reference::Column::Kind.eq(ConfigKind::Branch))
        .one(conn)
        .await
        .map_err(|_| {
            CliError::fatal(
                "agent doctor could not inspect the frozen intent ref; check the repository database and rerun doctor"
                    .to_string(),
            )
        })?
        .map(|_| crate::internal::ai::history::AI_REF.to_string());
    Ok(LegacyCodeResidue {
        paths,
        intent_ref,
        note: LEGACY_CODE_RESIDUE_NOTE,
    })
}

pub async fn execute_safe(args: DoctorArgs, output: &OutputConfig) -> CliResult<()> {
    // `agent doctor` is the corruption diagnostic itself. It must return a
    // controlled, path-free error when the database is unavailable instead
    // of reaching the legacy panic convenience wrapper. The CLI preflight has
    // already resolved repository storage through the shared
    // `LBR-REPO-001` / `LBR-REPO-003` mapping; this fallback keeps the same
    // stable codes for in-process callers.
    let storage = util::try_get_storage_path(None).map_err(doctor_storage_resolution_error)?;
    let conn = get_db_conn_instance_for_path(&storage.join(util::DATABASE))
        .await
        .map_err(|error| doctor_database_open_error(error.kind()))?;
    pin_doctor_hash_kind(&conn).await?;
    let schema_present = table_exists(&conn, "agent_session").await?
        && table_exists(&conn, "agent_checkpoint").await?;

    let (active_sessions, stopped_sessions, orphan_checkpoints) = if schema_present {
        let active = scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_session WHERE state = 'active'",
        )
        .await?;
        let stopped = scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_session WHERE state = 'stopped'",
        )
        .await?;
        // Orphan = checkpoint rows whose session_id no longer joins (would
        // imply CASCADE failed or the row was hand-written). Should be 0
        // under normal operation; surfacing >0 is a real diagnostic.
        //
        // Direction matters (agent.md orphan rules): ONLY
        // checkpoint-without-session is illegal. The reverse —
        // session-without-checkpoint — is a legal intermediate state
        // (active session before its first TurnEnd/Stop) and must never
        // be flagged, so no symmetric query exists here.
        let orphans = scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint cp \
             LEFT JOIN agent_session s ON s.session_id = cp.session_id \
             WHERE s.session_id IS NULL",
        )
        .await?;
        (active, stopped, orphans)
    } else {
        (0, 0, 0)
    };

    // Hook installation status across the v1 adapter matrix.
    // - claude-code and gemini carry dedicated HookProvider impls and
    //   report real install status.
    // - Stable-promoted adapters probe through their spec's AG-19
    //   `hooks` provider when they have one (codex, opencode — the A6.5
    //   smoke requires doctor to see all three first-batch chains);
    //   specs without an installable HookProvider (Cursor, Copilot,
    //   FactoryAi) stay `installed: None`.
    // - Any future preview adapters (PREVIEW_SPECS empty after Phase
    //   4.4) would surface here too.
    let mut provider_hooks = vec![
        check_provider(
            "claude-code",
            AgentStability::Stable,
            Some(claude_provider()),
        ),
        check_provider("gemini", AgentStability::Stable, Some(gemini_provider())),
    ];
    for spec in STABLE_PROMOTED_SPECS {
        provider_hooks.push(check_provider(
            spec.provider_name,
            AgentStability::Stable,
            spec.hooks,
        ));
    }
    for spec in PREVIEW_SPECS {
        provider_hooks.push(check_provider(
            spec.provider_name,
            AgentStability::Preview,
            None,
        ));
    }

    // AG-17/AG-20: gemini is uninstall-only. Fully-installed leftover hook
    // config means an old install was never removed — actionable hint.
    let gemini_hooks_remnant = provider_hooks
        .iter()
        .any(|ph| ph.name == "gemini" && ph.installed == Some(true));

    let checkpoint_store = scan_checkpoint_store(&conn, schema_present, args.repair).await?;
    let findings_store = scan_agent_findings(&conn, schema_present, args.repair).await?;
    let legacy_code_residue = scan_legacy_code_residue(&conn).await?;

    emit_report(
        &DoctorReport {
            schema_present,
            active_sessions,
            stopped_sessions,
            orphan_checkpoints,
            provider_hooks,
            gemini_hooks_remnant,
            checkpoint_store,
            findings_store,
            legacy_code_residue,
        },
        output,
    )
}

// ---------------------------------------------------------------------------
// AG-20 checkpoint-store/marker scan (repair classes + legacy-v1)
// ---------------------------------------------------------------------------

/// One `agent_checkpoint` row as loaded for the scan (only the columns the
/// checkpoint-object classes actually compare — doctor is metadata-first and never
/// loads transcript blobs).
#[derive(Debug, Clone)]
struct CatalogRow {
    checkpoint_id: String,
    tree_oid: String,
    metadata_blob_oid: String,
    traces_commit: String,
}

/// One ref-reachable checkpoint, attributed to the first-parent commit
/// that introduced it (the commit whose parent tree lacks the id).
#[derive(Debug, Clone)]
struct RefCheckpoint {
    /// The introducing commit on `refs/libra/traces` (what
    /// `agent_checkpoint.traces_commit` must equal).
    commit: String,
    /// That commit's root tree (what `agent_checkpoint.tree_oid` must equal).
    root_tree: String,
    /// `checkpoint/<p>/<rest>/metadata.json` blob OID, when the inner tree
    /// was readable and carries the entry.
    metadata_blob: Option<String>,
    /// `manifest.json` present in the inner tree ⇒ AG-20 (E4-libra) layout.
    manifest_present: bool,
    /// `metadata.json` present in the inner tree.
    metadata_present: bool,
    /// Whether the inner checkpoint tree could be read at all.
    inner_readable: bool,
    /// `Libra-Parent-Commit` trailer from the introducing commit message.
    parent_commit_trailer: Option<String>,
    /// `Libra-Scope` trailer from the introducing commit message.
    scope_trailer: Option<String>,
}

impl RefCheckpoint {
    /// Legacy-v1 layout: readable inner tree with `metadata.json` but no
    /// `manifest.json` (pre-AG-20 writer output).
    fn is_legacy_v1(&self) -> bool {
        self.inner_readable && self.metadata_present && !self.manifest_present
    }
}

/// Fields doctor needs from a checkpoint's `metadata.json`. Both external
/// schema shapes parse into this: v1 (pre-AG-20) and v2 (AG-20, adds
/// `model`) carry `session_id`, `created_at` and `scope`; unknown fields
/// are ignored.
#[derive(Debug, serde::Deserialize)]
struct CheckpointMetadataProbe {
    session_id: String,
    created_at: i64,
    #[serde(default)]
    scope: Option<String>,
    // A0-02: subagent-scope checkpoints carry these linkage fields flat in
    // `metadata.json` so a class-2 (crash-window-B) repair can rebuild a
    // first-class `scope='subagent'` catalog row instead of leaving it manual.
    #[serde(default)]
    parent_checkpoint_id: Option<String>,
    #[serde(default)]
    subagent_session_id: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    subagent: Option<SubagentMetadataProbe>,
}

#[derive(Debug, serde::Deserialize)]
struct SubagentMetadataProbe {
    #[serde(default)]
    provenance: Option<String>,
}

/// `Libra-*` trailers from a traces checkpoint commit message.
#[derive(Debug, Default)]
struct LibraTrailers {
    parent_commit: Option<String>,
    scope: Option<String>,
}

fn parse_libra_trailers(message: &str) -> LibraTrailers {
    let mut trailers = LibraTrailers::default();
    for line in message.lines() {
        if let Some(value) = line.strip_prefix("Libra-Parent-Commit: ") {
            trailers.parent_commit = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Libra-Scope: ") {
            trailers.scope = Some(value.trim().to_string());
        }
    }
    trailers
}

/// Thin, no-write reader over loose objects owned by this repository.
///
/// Doctor is a diagnostic boundary: it must not consult alternates, parse
/// `LIBRA_STORAGE_*`, enumerate packs, or rebuild a missing pack index.
/// Those generic storage paths can expose foreign filesystem details through
/// tracing and can mutate a repository during a nominally read-only scan.
/// A packed or remote-only object is therefore deliberately reported as
/// unavailable until a normal storage command restores a local loose copy.
struct ObjectReader {
    repo_path: PathBuf,
}

impl ObjectReader {
    fn secure_reads_supported() -> bool {
        cfg!(unix)
    }

    fn open_loose(&self, oid: &ObjectHash) -> std::io::Result<std::fs::File> {
        crate::utils::object::open_local_loose_object_no_follow(&self.repo_path, oid)
    }

    fn exists(&self, oid: &ObjectHash) -> bool {
        self.open_loose(oid).is_ok()
    }

    fn exists_str(&self, oid: &str) -> bool {
        crate::internal::object_format::parse_repo_oid(oid)
            .map(|hash| self.exists(&hash))
            .unwrap_or(false)
    }

    fn read_typed(&self, oid: &ObjectHash) -> anyhow::Result<(String, Vec<u8>)> {
        let file = self
            .open_loose(oid)
            .map_err(|_| anyhow::anyhow!("local object is unavailable"))?;
        crate::utils::object::read_git_object_bounded_validated_file(
            file,
            oid,
            DOCTOR_LOCAL_OBJECT_READ_CAP_BYTES,
        )
        .map_err(|_| anyhow::anyhow!("local object could not be read"))
    }

    fn read_raw(&self, oid: &ObjectHash) -> anyhow::Result<Vec<u8>> {
        self.read_typed(oid).map(|(_, bytes)| bytes)
    }

    /// Return a size only after validating the entire held loose-object
    /// stream against its content-addressed OID. The payload is streamed in a
    /// fixed buffer rather than returned, so index repair never adopts a
    /// mutable manifest `byte_len` or materializes transcript contents.
    fn validated_size(
        &self,
        oid: &ObjectHash,
        expected_type: &str,
        max_content_bytes: u64,
    ) -> anyhow::Result<i64> {
        let file = self
            .open_loose(oid)
            .map_err(|_| anyhow::anyhow!("local object is unavailable"))?;
        let (actual_type, size) =
            crate::utils::object::validate_git_object_streaming_file(file, oid, max_content_bytes)
                .map_err(|_| anyhow::anyhow!("local object could not be integrity-checked"))?;
        if actual_type != expected_type {
            bail!("local object has an unexpected type");
        }
        i64::try_from(size).map_err(|_| anyhow::anyhow!("local object size exceeds index range"))
    }

    fn read_commit(&self, oid: &ObjectHash) -> anyhow::Result<Commit> {
        let (object_type, data) = self.read_typed(oid)?;
        if object_type != "commit" {
            bail!("local object has an unexpected type");
        }
        Commit::from_bytes(&data, *oid)
            .map_err(|_| anyhow::anyhow!("local commit object is malformed"))
    }

    fn read_tree(&self, oid: &ObjectHash) -> anyhow::Result<Tree> {
        let (object_type, data) = self.read_typed(oid)?;
        if object_type != "tree" {
            bail!("local object has an unexpected type");
        }
        Tree::from_bytes(&data, *oid).map_err(|_| anyhow::anyhow!("local tree object is malformed"))
    }
}

/// Aggregate budget for descriptor-pinned object validation. A validation
/// request uses at most the remaining global budget, so a large declared
/// object cannot consume more than this pass is allowed to inspect.
#[derive(Debug)]
struct ObjectIndexValidationBudget {
    remaining_bytes: u64,
}

impl ObjectIndexValidationBudget {
    fn new() -> Self {
        Self {
            remaining_bytes: DOCTOR_OBJECT_INDEX_VALIDATION_TOTAL_CAP_BYTES,
        }
    }

    fn validate(
        &mut self,
        reader: &ObjectReader,
        oid: &ObjectHash,
        expected_type: &str,
    ) -> anyhow::Result<i64> {
        if self.remaining_bytes == 0 {
            bail!("object-index validation budget is exhausted");
        }
        let maximum = self
            .remaining_bytes
            .min(DOCTOR_STREAMING_OBJECT_VALIDATION_CAP_BYTES);
        let size = reader.validated_size(oid, expected_type, maximum)?;
        let consumed =
            u64::try_from(size).map_err(|_| anyhow::anyhow!("validated object size is invalid"))?;
        self.remaining_bytes = self
            .remaining_bytes
            .checked_sub(consumed)
            .context("object-index validation budget was exceeded")?;
        Ok(size)
    }
}

/// One object in an E4 checkpoint's reachability set, tagged with the
/// o_type the writer's `object_index` enqueue path stamps on it
/// (`history.rs::append_checkpoint_commit` / `splice_checkpoint_tree`):
/// every tree (root, `checkpoint/`, prefix, inner, `events/`,
/// `transcript/`) is `tree`, transcript files (including E5 chunks) are
/// `agent_transcript`, and the JSON/text sidecars plus lifecycle events
/// are `blob`.
#[derive(Debug)]
struct E4Object {
    /// A fixed role label for findings. This never comes from a tree-entry
    /// name or manifest path: a damaged checkpoint can otherwise turn a
    /// diagnostic into a transcript/metadata disclosure channel.
    label: String,
    oid: String,
    o_type: &'static str,
    /// The fixed checkpoint-tree role that may be corroborated by a manifest
    /// entry. `None` means a structural tree/foreign entry that must never
    /// receive a manifest-provided size.
    manifest_role: Option<ManifestRole>,
}

/// Result of sweeping one E4 checkpoint's full object set (class 1
/// detection input; the `present` list doubles as the class-3 target set).
#[derive(Debug, Default)]
struct E4Sweep {
    /// Objects verified present (existence via read for trees, store
    /// probe for blobs), with writer-matching o_type.
    present: Vec<E4Object>,
    /// Content-free descriptors of missing/unreadable objects.
    missing: Vec<String>,
    /// The inner checkpoint tree carries the M4+ manifest entry. Legacy-v1
    /// checkpoints are deliberately outside automatic import replay repair.
    manifest_present: bool,
    /// Once the global reachability budget is exhausted, the sweep must stop
    /// issuing object reads/probes. The accompanying fixed finding suppresses
    /// all automatic index repair for this checkpoint.
    entry_limit_hit: bool,
}

impl E4Sweep {
    /// Record one object, deduplicating by OID (the manifest re-declares
    /// blobs the tree enumeration already visited).
    fn record(
        &mut self,
        seen: &mut BTreeSet<String>,
        label: &str,
        oid: &str,
        o_type: &'static str,
        manifest_role: Option<ManifestRole>,
        exists: bool,
    ) {
        if self.entry_limit_hit {
            return;
        }
        if !seen.insert(oid.to_string()) {
            return;
        }
        if self.present.len().saturating_add(self.missing.len()) >= DOCTOR_E4_OBJECT_CAP {
            self.record_tree_entry_limit();
            return;
        }
        if exists {
            self.present.push(E4Object {
                label: label.to_string(),
                oid: oid.to_string(),
                o_type,
                manifest_role,
            });
        } else {
            self.missing
                .push(format!("{label} {}", diagnostic_oid(oid)));
        }
    }

    fn record_invalid_manifest_object(&mut self) {
        self.missing
            .push("manifest-declared object has an invalid object identifier".to_string());
    }

    fn record_invalid_manifest_declaration(&mut self) {
        self.missing
            .push("manifest contains an invalid object declaration".to_string());
    }

    fn record_manifest_tree_mismatch(&mut self) {
        self.missing
            .push("manifest-declared object does not match the checkpoint tree".to_string());
    }

    fn record_tree_entry_limit(&mut self) {
        if !self.entry_limit_hit {
            self.entry_limit_hit = true;
            self.missing.push(
                "checkpoint tree exceeds the doctor entry limit; manual review is required"
                    .to_string(),
            );
        }
    }

    fn record_manifest_declaration_limit(&mut self) {
        self.missing.push(
            "checkpoint manifest exceeds the doctor declaration limit; manual review is required"
                .to_string(),
        );
    }
}

/// Return an OID only after validating its exact repository grammar. OIDs are
/// safe structural diagnostics; arbitrary strings from a damaged manifest or
/// catalog are not.
fn diagnostic_oid(value: &str) -> String {
    crate::internal::object_format::parse_repo_oid(value)
        .map(|oid| oid.to_string())
        .unwrap_or_else(|_| "invalid object identifier".to_string())
}

/// Return a checkpoint identity only if it is the writer's canonical UUID
/// spelling. Ref tree entry names are untrusted too: callers must validate
/// before using one as a catalog key or repair input.
fn canonical_checkpoint_id(value: &str) -> Option<String> {
    let parsed = uuid::Uuid::parse_str(value).ok()?;
    let canonical = parsed.hyphenated().to_string();
    (value == canonical).then_some(canonical)
}

/// `checkpoint_id` is normally a writer-generated UUID, but a damaged
/// catalog or ref tree can contain arbitrary bytes. It reaches both human and
/// JSON doctor reports (and repair spans), so accept only the canonical UUID
/// spelling; anything else gets a fixed non-correlatable label.
fn diagnostic_checkpoint_id(value: &str) -> String {
    canonical_checkpoint_id(value).unwrap_or_else(|| "checkpoint-id-redacted".to_string())
}

/// `object_index.o_type` is mutable database content, not a trusted enum.
/// Keep the public writer taxonomy readable while refusing to echo arbitrary
/// damaged-row strings in human or JSON diagnostics.
fn diagnostic_object_index_type(value: &str) -> &'static str {
    match value {
        "commit" => "commit",
        "tree" => "tree",
        "blob" => "blob",
        "agent_transcript" => "agent_transcript",
        _ => "unrecognized type",
    }
}

/// `object_index.o_type` has one capture-specific tag for transcript blobs;
/// the loose-object header still uses Git's ordinary `blob` type.
fn expected_git_object_type(index_type: &str) -> &'static str {
    match index_type {
        "commit" => "commit",
        "tree" => "tree",
        "blob" | "agent_transcript" => "blob",
        // All callers supply the fixed writer vocabulary. Keeping this
        // fallback fail-closed avoids accidentally treating a future mutable
        // database value as a valid loose-object kind.
        _ => "invalid",
    }
}

/// Fallback mapping for a repository-resolution failure reached without the
/// CLI preflight. A genuine NotFound keeps the shared `LBR-REPO-001`
/// contract; every other resolution failure (detached, migrating or corrupt
/// linked worktree) keeps `LBR-REPO-003` with a path-free remedy.
fn doctor_storage_resolution_error(error: std::io::Error) -> CliError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return CliError::repo_not_found();
    }
    CliError::fatal(
        "agent doctor could not resolve repository storage; from the main worktree run \
         `libra worktree repair --confirm <worktree-path>` (or re-add the worktree), then \
         rerun doctor",
    )
    .with_stable_code(StableErrorCode::RepoStateInvalid)
}

/// Path-free mapping for a failed repository-database open. The stable codes
/// match the generic repository preflight that doctor deliberately skips: a
/// missing database is `LBR-REPO-002`, any other open failure (permissions,
/// corruption, a schema written by a newer Libra) is `LBR-IO-001`.
fn doctor_database_open_error(kind: std::io::ErrorKind) -> CliError {
    if kind == std::io::ErrorKind::NotFound {
        return CliError::fatal(
            "repository database not found; restore the repository's .libra storage (for \
             example from a backup) and rerun `libra agent doctor`",
        )
        .with_stable_code(StableErrorCode::RepoCorrupt);
    }
    CliError::fatal(
        "agent doctor could not open the repository database; if it was written by a newer \
         Libra, install a newer Libra binary; otherwise restore repository storage, then rerun \
         doctor",
    )
    .with_stable_code(StableErrorCode::IoReadFailed)
}

/// Doctor bypasses the generic CLI database preflight so it can render a
/// path-free database failure. Once its explicit open succeeds, it still must
/// pin the repository object format before reading traces objects. The stable
/// codes match that preflight (`LBR-IO-001` for an unreadable value,
/// `LBR-REPO-002` for an unsupported one); the stored value is never echoed.
async fn pin_doctor_hash_kind(conn: &DatabaseConnection) -> CliResult<()> {
    let object_format = ConfigKv::get_with_conn(conn, "core.objectformat")
        .await
        .map_err(|_| {
            CliError::fatal(
                "agent doctor could not read repository object format; repair the repository database and rerun doctor"
                    .to_string(),
            )
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?
        .map(|entry| entry.value)
        .unwrap_or_else(|| "sha1".to_string());
    let hash_kind = crate::internal::object_format::parse_config_value(&object_format).map_err(|_| {
        CliError::fatal(
            "agent doctor found an unsupported object format in the repository configuration; repair core.objectformat and rerun doctor"
                .to_string(),
        )
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    set_hash_kind(hash_kind);
    Ok(())
}

/// Keep tree-entry names out of doctor output. The writer emits this small,
/// fixed layout, while foreign names in a damaged tree have no diagnostic
/// authority and must not be reflected back to the terminal or JSON.
fn checkpoint_inner_entry_label(name: &str, is_tree: bool) -> &'static str {
    match (name, is_tree) {
        ("metadata.json", false) => "metadata.json",
        ("manifest.json", false) => "manifest.json",
        ("redaction_report.json", false) => "redaction_report.json",
        ("content_hash.txt", false) => "content_hash.txt",
        ("events", true) => "events tree",
        ("transcript", true) => "transcript tree",
        (_, true) => "unrecognized checkpoint subtree",
        (_, false) => "unrecognized checkpoint sidecar",
    }
}

/// Classify one manifest declaration without trusting its caller-controlled
/// role/path spelling. The label is intentionally a fixed vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ManifestRole {
    Metadata,
    LifecycleEvents,
    Transcript,
    RedactionReport,
    ContentHash,
}

fn manifest_role_from_key(role: &str) -> Option<ManifestRole> {
    match role {
        "metadata" => Some(ManifestRole::Metadata),
        "lifecycle_events" => Some(ManifestRole::LifecycleEvents),
        "transcript" => Some(ManifestRole::Transcript),
        "redaction_report" => Some(ManifestRole::RedactionReport),
        "content_hash" => Some(ManifestRole::ContentHash),
        _ => None,
    }
}

fn checkpoint_manifest_role_for_sidecar(name: &str) -> Option<ManifestRole> {
    match name {
        "metadata.json" => Some(ManifestRole::Metadata),
        "redaction_report.json" => Some(ManifestRole::RedactionReport),
        "content_hash.txt" => Some(ManifestRole::ContentHash),
        _ => None,
    }
}

fn checkpoint_manifest_role_for_subtree_entry(
    parent: &str,
    name: &str,
    is_tree: bool,
) -> Option<ManifestRole> {
    if is_tree {
        return None;
    }
    match parent {
        "transcript" => Some(ManifestRole::Transcript),
        "events" if name == "lifecycle.jsonl" => Some(ManifestRole::LifecycleEvents),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestDeclaredBlob {
    label: String,
    role: Option<ManifestRole>,
    oid: Option<String>,
    o_type: &'static str,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ManifestDeclarations {
    declarations: Vec<ManifestDeclaredBlob>,
    over_limit: bool,
}

fn manifest_declared_label(role: &str, part_index: Option<usize>) -> String {
    match (role, part_index) {
        ("metadata", None) => "manifest-declared metadata".to_string(),
        ("lifecycle_events", None) => "manifest-declared lifecycle events".to_string(),
        ("transcript", None) => "manifest-declared transcript".to_string(),
        ("transcript", Some(index)) => {
            format!("manifest-declared transcript part {}", index + 1)
        }
        ("redaction_report", None) => "manifest-declared redaction report".to_string(),
        ("content_hash", None) => "manifest-declared content hash".to_string(),
        (_, _) => "manifest-declared object".to_string(),
    }
}

fn manifest_declared_blob(
    role: &str,
    part_index: Option<usize>,
    oid: &str,
) -> ManifestDeclaredBlob {
    ManifestDeclaredBlob {
        label: manifest_declared_label(role, part_index),
        role: manifest_role_from_key(role),
        oid: crate::internal::object_format::parse_repo_oid(oid)
            .ok()
            .map(|parsed| parsed.to_string()),
        o_type: if role == "transcript" {
            "agent_transcript"
        } else {
            "blob"
        },
    }
}

/// Sweep every object belonging to one E4 checkpoint: walk
/// `root → checkpoint → <prefix> → <rest>`, enumerate the inner tree
/// (top-level sidecar blobs plus the `events/` and `transcript/`
/// subtrees), then cross-check the manifest's declared entries. The tree
/// enumeration is the primary probe — a missing `manifest.json` is
/// recorded like any other missing sidecar and never hides the rest. A
/// manifest may corroborate the size of a matching writer-owned tree entry,
/// but it can never add a new object to the reachability/index-repair set.
fn sweep_e4_checkpoint_objects(
    reader: &ObjectReader,
    root_tree_oid: &str,
    checkpoint_id: &str,
) -> E4Sweep {
    let mut sweep = E4Sweep::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    let (Some(prefix), Some(rest)) = (checkpoint_id.get(..2), checkpoint_id.get(2..)) else {
        sweep
            .missing
            .push("checkpoint tree path cannot be resolved from its identifier".to_string());
        return sweep;
    };

    // Chain walk: each level is both recorded (writer enqueues every one
    // of these trees as o_type "tree") and descended into. A missing or
    // unreadable tree ends the walk — everything below it is unreachable
    // and the finding already names the break point.
    let labels = [
        "root checkpoint tree",
        "checkpoint tree",
        "checkpoint prefix tree",
        "checkpoint leaf tree",
    ];
    let mut oid = root_tree_oid.to_string();
    let mut inner_tree: Option<Tree> = None;
    for (depth, label) in labels.iter().enumerate() {
        let tree = match crate::internal::object_format::parse_repo_oid(&oid) {
            Ok(hash) => match reader.read_tree(&hash) {
                Ok(tree) => {
                    if tree.tree_items.len() > DOCTOR_E4_TREE_ENTRY_CAP {
                        sweep.record_tree_entry_limit();
                        return sweep;
                    }
                    sweep.record(&mut seen, label, &oid, "tree", None, true);
                    tree
                }
                Err(_) => {
                    sweep.record(&mut seen, label, &oid, "tree", None, false);
                    return sweep;
                }
            },
            Err(_) => {
                sweep.record(&mut seen, label, &oid, "tree", None, false);
                return sweep;
            }
        };
        if depth == labels.len() - 1 {
            inner_tree = Some(tree);
            break;
        }
        let next_name = match depth {
            0 => "checkpoint",
            1 => prefix,
            _ => rest,
        };
        let Some(entry) = tree
            .tree_items
            .iter()
            .find(|item| item.name == next_name && item.mode == TreeItemMode::Tree)
        else {
            sweep.missing.push(format!(
                "{} is absent from the checkpoint tree",
                labels[depth + 1]
            ));
            return sweep;
        };
        oid = entry.id.to_string();
    }
    let Some(inner) = inner_tree else {
        return sweep;
    };

    // Read the small manifest before the tree enumeration, then bind its
    // declarations only to entries proven by that enumeration. A manifest is
    // untrusted metadata: it may supply a size for its matching role, never
    // introduce a new cloud-index target or override a tree role/type.
    let mut declared = ManifestDeclarations::default();
    let manifest_item = inner
        .tree_items
        .iter()
        .find(|item| item.name == "manifest.json" && item.mode != TreeItemMode::Tree);
    sweep.manifest_present = manifest_item.is_some();
    if let Some(manifest_item) = manifest_item
        && let Ok(bytes) = reader.read_raw(&manifest_item.id)
        && let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        declared = manifest_declared_blobs(&manifest);
    }
    if declared.over_limit {
        sweep.record_manifest_declaration_limit();
    }

    // Inner tree enumeration (the primary probe): sidecar blobs at the top
    // level and exactly the two writer-owned subtrees (`events/` and
    // `transcript/`). Do not recursively inspect arbitrary foreign subtrees:
    // they are not part of the checkpoint protocol and a damaged inner tree
    // could otherwise amplify this foreground scan by another full tree cap
    // per entry. Transcript blobs — including E5 chunk parts — carry the
    // writer's distinguished "agent_transcript" tag.
    for item in &inner.tree_items {
        if sweep.entry_limit_hit {
            break;
        }
        let item_oid = item.id.to_string();
        if item.mode == TreeItemMode::Tree {
            let label = checkpoint_inner_entry_label(&item.name, true);
            // Foreign/additive trees have no stable writer semantics below
            // their root. Record the tree itself, but never fan out into
            // untrusted descendants.
            if item.name != "events" && item.name != "transcript" {
                sweep.record(&mut seen, label, &item_oid, "tree", None, true);
                continue;
            }
            match reader.read_tree(&item.id) {
                Ok(subtree) => {
                    sweep.record(&mut seen, label, &item_oid, "tree", None, true);
                    if subtree.tree_items.len() > DOCTOR_E4_TREE_ENTRY_CAP {
                        sweep.record_tree_entry_limit();
                        continue;
                    }
                    let leaf_type = if item.name == "transcript" {
                        "agent_transcript"
                    } else {
                        "blob"
                    };
                    for leaf in &subtree.tree_items {
                        if sweep.entry_limit_hit {
                            break;
                        }
                        let leaf_oid = leaf.id.to_string();
                        let leaf_label = match item.name.as_str() {
                            "transcript" => "transcript entry",
                            "events" => "lifecycle event entry",
                            _ => "checkpoint subtree entry",
                        };
                        let o_type = if leaf.mode == TreeItemMode::Tree {
                            "tree"
                        } else {
                            leaf_type
                        };
                        sweep.record(
                            &mut seen,
                            leaf_label,
                            &leaf_oid,
                            o_type,
                            checkpoint_manifest_role_for_subtree_entry(
                                &item.name,
                                &leaf.name,
                                leaf.mode == TreeItemMode::Tree,
                            ),
                            reader.exists_str(&leaf_oid),
                        );
                    }
                }
                Err(_) => sweep.record(&mut seen, label, &item_oid, "tree", None, false),
            }
        } else {
            sweep.record(
                &mut seen,
                checkpoint_inner_entry_label(&item.name, false),
                &item_oid,
                "blob",
                checkpoint_manifest_role_for_sidecar(&item.name),
                reader.exists_str(&item_oid),
            );
        }
    }

    // Manifest cross-check: accept a declaration only when the same OID,
    // fixed role, and writer object type were all enumerated from the
    // checkpoint tree. Extra/mislabelled declarations are corruption
    // findings, not reachability evidence: otherwise `--repair` could place
    // an attacker-selected existing blob into object_index for cloud sync.
    // The map keeps this O(tree entries + declarations), rather than scanning
    // the complete reachability set once per untrusted declaration.
    let mut tree_backed: BTreeMap<(String, ManifestRole, &'static str), usize> = BTreeMap::new();
    for (index, object) in sweep.present.iter().enumerate() {
        if let Some(role) = object.manifest_role {
            tree_backed.insert((object.oid.clone(), role, object.o_type), index);
        }
    }
    let mut seen_declarations: BTreeSet<(String, ManifestRole)> = BTreeSet::new();
    for declaration in declared.declarations {
        let (Some(declared_oid), Some(role)) = (declaration.oid.as_deref(), declaration.role)
        else {
            sweep.record_invalid_manifest_object();
            continue;
        };
        if !seen_declarations.insert((declared_oid.to_string(), role)) {
            sweep.record_invalid_manifest_declaration();
            continue;
        }
        if !tree_backed.contains_key(&(declared_oid.to_string(), role, declaration.o_type)) {
            sweep.record_manifest_tree_mismatch();
        }
    }

    sweep
}

/// Extract checkpoint manifest declarations using only the writer's fixed
/// role vocabulary. The manifest is corroborating metadata only: its OIDs
/// may be matched to a tree-backed role, but its `byte_len` is deliberately
/// ignored because object-index repair derives size from an integrity-checked
/// loose object stream instead.
///
/// Both the known role surface and the total declarations are bounded. A
/// damaged 4 MiB JSON object can otherwise contain enough syntactically valid
/// parts to turn a foreground doctor run into an unbounded/O(n²) operation.
fn manifest_declared_blobs(manifest: &serde_json::Value) -> ManifestDeclarations {
    let Some(entries) = manifest.get("entries").and_then(|value| value.as_object()) else {
        return ManifestDeclarations::default();
    };
    let mut out = ManifestDeclarations {
        declarations: Vec::new(),
        over_limit: entries.len() > 5,
    };
    for role in [
        "metadata",
        "lifecycle_events",
        "transcript",
        "redaction_report",
        "content_hash",
    ] {
        let Some(entry) = entries.get(role) else {
            continue;
        };
        if let Some(oid) = entry.get("oid").and_then(|value| value.as_str()) {
            if out.declarations.len() == DOCTOR_E4_MANIFEST_DECLARATION_CAP {
                out.over_limit = true;
                return out;
            }
            out.declarations
                .push(manifest_declared_blob(role, None, oid));
        }
        // Only transcript entries use E5 parts. Treat all other `parts`
        // fields as untrusted extension data, not repair evidence.
        if role != "transcript" {
            continue;
        }
        let Some(parts) = entry.get("parts").and_then(|value| value.as_array()) else {
            continue;
        };
        if parts.len() > DOCTOR_E4_MANIFEST_DECLARATION_CAP {
            out.over_limit = true;
        }
        for (part_index, part) in parts
            .iter()
            .take(DOCTOR_E4_MANIFEST_DECLARATION_CAP)
            .enumerate()
        {
            let Some(oid) = part.get("oid").and_then(|value| value.as_str()) else {
                continue;
            };
            if out.declarations.len() == DOCTOR_E4_MANIFEST_DECLARATION_CAP {
                out.over_limit = true;
                return out;
            }
            out.declarations
                .push(manifest_declared_blob(role, Some(part_index), oid));
        }
    }
    out
}

/// Run only the bounded, automatic pending-artifact replay path for the
/// detached SessionStart worker. This skips doctor classification and every
/// unrelated repair family.
pub(crate) async fn run_pending_artifact_worker() -> CliResult<()> {
    let Ok(storage) = util::try_get_storage_path(None) else {
        return Ok(());
    };
    let database_path = storage.join(util::DATABASE);
    let Some(database_path) = database_path.to_str() else {
        return Ok(());
    };
    let Ok(conn) = crate::internal::db::open_connection_without_schema_management(
        database_path,
        Duration::from_millis(200),
    )
    .await
    else {
        return Ok(());
    };
    let schema_present = match (
        table_exists(&conn, "agent_session").await,
        table_exists(&conn, "agent_checkpoint").await,
    ) {
        (Ok(true), Ok(true)) => true,
        (Ok(false), _) | (_, Ok(false)) => false,
        _ => {
            tracing::warn!(
                target: "agent.capture.recovery",
                reason = "worker_schema_probe_failed",
                "automatic capture recovery stopped before a schema could be verified"
            );
            return Ok(());
        }
    };
    if !schema_present {
        return Ok(());
    }
    if pin_doctor_hash_kind(&conn).await.is_err() {
        return Ok(());
    }
    let observed_at_ms = Utc::now().timestamp_millis();
    let repo = match crate::internal::workspace::RepoIdentity::resolve(&conn).await {
        Ok(repo) => repo,
        Err(_) => return Ok(()),
    };
    let root = util::request_working_dir();
    let scope = match crate::internal::ai::capture_scope::CaptureScope::resolve(&conn, &root).await
    {
        Ok(scope) if scope.repo_id == repo.as_str() => scope,
        _ => return Ok(()),
    };
    // An expired or released workspace fence gives this worker no authority
    // to mutate candidates. Return before queueing so an early stale candidate
    // cannot block later worktrees or candidates until the lease is renewed.
    if scope.assert_workspace_fence_live(&conn).await.is_err() {
        return Ok(());
    }
    let catalog = CaptureCatalogStore::new(conn.clone());
    let mut batch = ArtifactReplayBatch::new();
    let deadline = match batch.deadline() {
        Ok(deadline) => deadline.monotonic(),
        Err(_) => return Ok(()),
    };
    for _ in 0..=pending::MAX_ARTIFACTS {
        if std::time::Instant::now() >= deadline {
            break;
        }
        let queue = match conn.begin().await {
            Ok(txn) => match pending::pending_candidates_for_scope(&txn, &scope).await {
                Ok(queue) => match txn.commit().await {
                    Ok(()) => queue,
                    Err(_) => return Ok(()),
                },
                Err(_) => {
                    let _ = txn.rollback().await;
                    return Ok(());
                }
            },
            Err(_) => return Ok(()),
        };
        let mut made_progress = queue.quarantined > 0;
        let mut stop_after_round = false;
        if queue.headers.is_empty() && !made_progress {
            break;
        }
        for header in queue.headers {
            if std::time::Instant::now() >= deadline
                || scope.assert_workspace_fence_live(&conn).await.is_err()
            {
                tracing::warn!(target: "agent.capture.recovery", reason = "workspace_lease_expired", "capture recovery stopped because the current workspace lease is no longer live");
                stop_after_round = true;
                break;
            }
            let recovery = match catalog
                .pending_finalizer_recovery_for_header(
                    &header,
                    &storage,
                    &root,
                    observed_at_ms,
                    deadline,
                )
                .await
            {
                Ok(Some(recovery)) => recovery,
                Ok(None) => {
                    if !quarantine_worker_candidate(
                        &conn,
                        &scope,
                        &header.binding.checkpoint_id,
                        Some(deadline),
                    )
                    .await
                    {
                        tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_quarantine_failed", "capture recovery stopped because an unresolvable candidate could not be retained in quarantine");
                        stop_after_round = true;
                        break;
                    }
                    made_progress = true;
                    tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_unresolvable", "capture recovery candidate was retained in quarantine");
                    continue;
                }
                Err(_) if std::time::Instant::now() >= deadline => {
                    tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_deadline", "capture recovery stopped after its shared batch deadline elapsed");
                    stop_after_round = true;
                    break;
                }
                Err(error) if is_permanent_worker_candidate_error(&error) => {
                    if !quarantine_worker_candidate(
                        &conn,
                        &scope,
                        &header.binding.checkpoint_id,
                        Some(deadline),
                    )
                    .await
                    {
                        tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_quarantine_failed", "capture recovery stopped because an invalid candidate could not be retained in quarantine");
                        stop_after_round = true;
                        break;
                    }
                    made_progress = true;
                    tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_invalid", "capture recovery candidate was retained in quarantine");
                    continue;
                }
                Err(_) => {
                    tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_lookup_failed", "capture recovery skipped a candidate after a transient lookup failure");
                    continue;
                }
            };
            if !recovery.artifact_present() {
                continue;
            }
            if recovery.superseded() || recovery.quarantined() || recovery.budget_exhausted() {
                if !quarantine_worker_candidate(
                    &conn,
                    &scope,
                    recovery.checkpoint_id(),
                    Some(deadline),
                )
                .await
                {
                    tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_quarantine_failed", "capture recovery stopped because a stale candidate could not be retained in quarantine");
                    stop_after_round = true;
                    break;
                }
                made_progress = true;
                continue;
            }
            let mut finding = CheckpointFinding {
                inconsistency_type: CLASS_EXPIRED_INFLIGHT_MARKER.to_string(),
                checkpoint_id: diagnostic_checkpoint_id(recovery.checkpoint_id()),
                detail: "bounded automatic capture recovery".to_string(),
                repaired: false,
                manual_required: false,
            };
            let replay_disposition = replay_capture_artifact(
                &conn,
                &mut finding,
                &recovery,
                observed_at_ms,
                false,
                &mut batch,
            )
            .await;
            match replay_disposition {
                ArtifactReplayDisposition::PermanentFailure => {
                    if !quarantine_worker_candidate(
                        &conn,
                        &scope,
                        recovery.checkpoint_id(),
                        Some(deadline),
                    )
                    .await
                    {
                        tracing::warn!(target: "agent.capture.recovery", reason = "pending_candidate_quarantine_failed", "capture recovery stopped because a failed candidate could not be retained in quarantine");
                        stop_after_round = true;
                        break;
                    }
                    made_progress = true;
                }
                ArtifactReplayDisposition::Completed => made_progress = true,
                ArtifactReplayDisposition::RetryLater | ArtifactReplayDisposition::Refused => {
                    // Candidate-local retry/refusal must not block later
                    // snapshot entries; the attempt budget still bounds work.
                    continue;
                }
                ArtifactReplayDisposition::Deferred => {
                    stop_after_round = true;
                    break;
                }
            }
            if !batch.has_capacity() {
                stop_after_round = true;
                break;
            }
        }
        if stop_after_round || !made_progress || !batch.has_capacity() {
            break;
        }
    }
    Ok(())
}

fn is_permanent_worker_candidate_error(
    error: &crate::internal::ai::capture::catalog::CaptureCatalogError,
) -> bool {
    use crate::internal::ai::capture::catalog::CaptureCatalogError;

    !matches!(
        error,
        CaptureCatalogError::TransactionStart
            | CaptureCatalogError::DeadlineExceeded
            | CaptureCatalogError::CommitFailed
            | CaptureCatalogError::Database
            | CaptureCatalogError::SchemaUnavailable
            | CaptureCatalogError::WorkspaceLeaseRejected
    )
}

async fn quarantine_worker_candidate(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    checkpoint_id: &str,
    deadline: Option<std::time::Instant>,
) -> bool {
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        return false;
    }
    let Ok(txn) = conn.begin().await else {
        return false;
    };
    if txn
        .execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .is_err()
        || scope.assert_workspace_fence_live(&txn).await.is_err()
        || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
    {
        let _ = txn.rollback().await;
        return false;
    }
    match pending::quarantine_checkpoint_if_present(&txn, &scope.repo_id, checkpoint_id).await {
        Ok(true) => txn.commit().await.is_ok(),
        Ok(false) => {
            let _ = txn.rollback().await;
            false
        }
        Err(_) => {
            let _ = txn.rollback().await;
            false
        }
    }
}

/// Retain a superseded artifact outside the eligible pending window without
/// authenticating or hydrating its payload. The scope fence is rechecked
/// before the exact current-scope pending header is moved.
async fn quarantine_stale_pending_artifact(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    checkpoint_id: &str,
) -> Result<()> {
    let txn = conn
        .begin()
        .await
        .context("start stale capture artifact quarantine")?;
    let result = async {
        txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
            .await
            .context("lock stale capture artifact quarantine")?;
        scope
            .assert_workspace_fence_live(&txn)
            .await
            .context("verify current capture workspace lease")?;
        let (header, namespace) =
            pending::header_for_checkpoint(&txn, &scope.repo_id, checkpoint_id)
                .await?
                .context("stale capture artifact header disappeared")?;
        anyhow::ensure!(
            namespace == crate::internal::metadata::MetadataScope::AgentCapturePending
                && header.binding.scope == *scope,
            "stale capture artifact scope or namespace changed"
        );
        anyhow::ensure!(
            pending::quarantine_checkpoint_if_present(&txn, &scope.repo_id, checkpoint_id).await?,
            "stale capture artifact could not be retained"
        );
        let (retained, namespace) =
            pending::header_for_checkpoint(&txn, &scope.repo_id, checkpoint_id)
                .await?
                .context("stale capture artifact disappeared during quarantine")?;
        anyhow::ensure!(
            namespace == crate::internal::metadata::MetadataScope::AgentCaptureQuarantine
                && retained.binding.scope == *scope,
            "stale capture artifact was not moved to its exact quarantine namespace"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        let _ = txn.rollback().await;
        return Err(error);
    }
    txn.commit()
        .await
        .context("commit stale capture artifact quarantine")
}

/// Run the AG-20 checkpoint-store and marker scan (and repairs when enabled).
///
/// The scan itself fails soft: object-store degradations become `note`
/// entries and under-detect instead of erroring, so `doctor` stays usable
/// on a damaged store. Database errors still propagate — they mean the
/// diagnosis itself cannot be trusted.
async fn scan_checkpoint_store(
    conn: &DatabaseConnection,
    schema_present: bool,
    repair: bool,
) -> CliResult<CheckpointStoreReport> {
    let mut report = CheckpointStoreReport {
        scanned: false,
        note: None,
        catalog_rows: 0,
        ref_reachable_checkpoints: 0,
        legacy_v1_checkpoints: 0,
        live_inflight_markers: 0,
        repair_applied: repair,
        repaired: 0,
        manual_required: 0,
        findings: Vec::new(),
    };
    if !schema_present {
        report.note = Some("agent schema not present (run `libra init`?)".to_string());
        return Ok(report);
    }
    let repo_path = match util::try_get_storage_path(None) {
        Ok(path) => path,
        Err(_) => {
            report.note = Some("repository storage unavailable".to_string());
            return Ok(report);
        }
    };
    report.scanned = true;
    let mut notes: Vec<String> = Vec::new();

    // Doctor deliberately reads only this repository's local object directory.
    // Following `objects/info/alternates` would make a malformed foreign
    // alternate path observable through the diagnostic/logging boundary. A
    // normal object command can restore a missing borrowed object locally;
    // until then doctor reports it conservatively as unavailable.
    let reader = ObjectReader {
        repo_path: repo_path.clone(),
    };
    if !ObjectReader::secure_reads_supported() {
        report.repair_applied = false;
        report.note = Some(
            "checkpoint object scan is unavailable because this platform lacks secure descriptor-relative reads; no repairs were applied"
                .to_string(),
        );
        return Ok(report);
    }
    let history = HistoryManager::for_traces(
        Arc::new(ClientStorage::init_local_existing(
            repo_path.join("objects"),
        )),
        repo_path.clone(),
        Arc::new(conn.clone()),
    );
    let mut findings: Vec<CheckpointFinding> = Vec::new();
    let mut plans: Vec<RepairPlan> = Vec::new();

    if table_exists(conn, "agent_coverage_claim").await? {
        let conflicts = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT c.coverage_schema_version, c.completeness, c.revision,
                        f.incoming_digest, f.incoming_source_channel,
                        f.incoming_observed_at, f.incoming_redaction_report_json
                 FROM agent_coverage_claim c
                 LEFT JOIN agent_coverage_conflict f
                   ON f.session_id = c.session_id
                  AND f.logical_turn_key = c.logical_turn_key
                  AND f.coverage_schema_version = c.coverage_schema_version
                 WHERE c.state = 'conflicted'
                 ORDER BY c.session_id, c.logical_turn_key, c.coverage_schema_version"
                    .to_string(),
            ))
            .await
            .map_err(|_| {
                CliError::fatal(
                    "agent doctor could not inspect conflicted coverage claims; check the repository database and rerun doctor"
                        .to_string(),
                )
            })?;
        for (ordinal, row) in conflicts.into_iter().enumerate() {
            let coverage_schema_version: i64 =
                row.try_get_by("coverage_schema_version").map_err(|_| {
                    CliError::fatal("agent doctor could not decode a coverage conflict".to_string())
                })?;
            let completeness: String = row.try_get_by("completeness").map_err(|_| {
                CliError::fatal("agent doctor could not decode a coverage conflict".to_string())
            })?;
            let revision: i64 = row.try_get_by("revision").map_err(|_| {
                CliError::fatal("agent doctor could not decode a coverage conflict".to_string())
            })?;
            let incoming_digest: Option<String> =
                row.try_get_by("incoming_digest").map_err(|_| {
                    CliError::fatal("agent doctor could not decode a coverage conflict".to_string())
                })?;
            let incoming_source_channel: Option<String> =
                row.try_get_by("incoming_source_channel").map_err(|_| {
                    CliError::fatal("agent doctor could not decode a coverage conflict".to_string())
                })?;
            let incoming_observed_at: Option<i64> =
                row.try_get_by("incoming_observed_at").map_err(|_| {
                    CliError::fatal("agent doctor could not decode a coverage conflict".to_string())
                })?;
            let incoming_redaction_report_json: Option<String> = row
                .try_get_by("incoming_redaction_report_json")
                .map_err(|_| {
                    CliError::fatal("agent doctor could not decode a coverage conflict".to_string())
                })?;
            let challenger = match (
                incoming_digest,
                incoming_source_channel,
                incoming_observed_at,
                incoming_redaction_report_json,
            ) {
                (Some(_), Some(_), Some(_), Some(_)) =>
                    "incoming redacted evidence and its redaction report are stored in agent_coverage_conflict".to_string(),
                _ => "incoming evidence is missing (legacy or externally damaged conflict row)"
                    .to_string(),
            };
            let completeness = match completeness.as_str() {
                "complete" => "complete",
                "partial" => "partial",
                "incomplete" => "incomplete",
                _ => "unrecognized",
            };
            findings.push(CheckpointFinding {
                inconsistency_type: CLASS_CONFLICTED_COVERAGE_CLAIM.to_string(),
                checkpoint_id: format!("coverage-conflict-{}", ordinal + 1),
                detail: format!(
                    "coverage conflict for coverage schema {coverage_schema_version} is parked at incumbent revision {revision} (completeness={completeness}); {challenger}; automatic resolution is unsafe because choosing either complete payload would discard provenance; inspect both durable candidates and resolve manually"
                ),
                repaired: false,
                manual_required: true,
            });
            plans.push(RepairPlan::Manual);
        }
    }

    if table_exists(conn, "agent_subagent_link").await? {
        let inconsistent = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT c.content_schema_version, c.current_revision,
                        c.current_checkpoint_id, c.current_digest,
                        r.checkpoint_id AS revision_checkpoint_id,
                        r.content_digest AS revision_digest,
                        cp.checkpoint_id AS catalog_checkpoint_id,
                        l.content_checkpoint_id AS link_checkpoint_id,
                        l.parent_session_id AS link_parent_session_id
                 FROM agent_subagent_content_claim c
                 LEFT JOIN agent_subagent_content_revision r
                   ON r.parent_session_id = c.parent_session_id
                  AND r.provider_kind = c.provider_kind
                  AND r.source_key = c.source_key
                  AND r.content_schema_version = c.content_schema_version
                  AND r.revision = c.current_revision
                 LEFT JOIN agent_checkpoint cp
                   ON cp.checkpoint_id = c.current_checkpoint_id
                  AND cp.session_id = c.parent_session_id
                  AND cp.scope = 'subagent'
                 LEFT JOIN agent_subagent_link l
                   ON l.content_checkpoint_id = c.current_checkpoint_id
                  AND l.parent_session_id = c.parent_session_id
                 WHERE c.current_revision > 0
                   AND (
                     c.current_checkpoint_id IS NULL OR c.current_digest IS NULL
                     OR r.checkpoint_id IS NULL OR r.content_digest IS NULL
                     OR r.checkpoint_id <> c.current_checkpoint_id
                     OR r.content_digest <> c.current_digest
                     OR cp.checkpoint_id IS NULL OR l.content_checkpoint_id IS NULL
                   )
                 ORDER BY c.parent_session_id, c.provider_kind, c.source_key,
                          c.content_schema_version"
                    .to_string(),
            ))
            .await
            .map_err(|_| {
                CliError::fatal(
                    "agent doctor could not inspect subagent content relations; check the repository database and rerun doctor"
                        .to_string(),
                )
            })?;
        for (ordinal, row) in inconsistent.into_iter().enumerate() {
            let schema_version: i64 = row.try_get_by("content_schema_version").map_err(|_| {
                CliError::fatal(
                    "agent doctor could not decode a subagent content relation".to_string(),
                )
            })?;
            let current_revision: i64 = row.try_get_by("current_revision").map_err(|_| {
                CliError::fatal(
                    "agent doctor could not decode a subagent content relation".to_string(),
                )
            })?;
            let checkpoint_id: Option<String> =
                row.try_get_by("current_checkpoint_id").map_err(|_| {
                    CliError::fatal(
                        "agent doctor could not decode a subagent content relation".to_string(),
                    )
                })?;
            findings.push(CheckpointFinding {
                inconsistency_type: CLASS_INCONSISTENT_SUBAGENT_CONTENT.to_string(),
                checkpoint_id: checkpoint_id
                    .as_deref()
                    .map(diagnostic_checkpoint_id)
                    .unwrap_or_else(|| format!("subagent-content-{}", ordinal + 1)),
                detail: format!(
                    "current subagent content relation for schema {schema_version} at revision {current_revision} is missing or disagrees across its claim, immutable revision, checkpoint catalog, and association link; replay is fail-closed and automatic reconstruction is unsafe"
                ),
                repaired: false,
                manual_required: true,
            });
            plans.push(RepairPlan::Manual);
        }

        let unresolved = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT l.content_checkpoint_id
                 FROM agent_subagent_link l
                 JOIN agent_subagent_content_claim c
                   ON c.parent_session_id = l.parent_session_id
                  AND c.current_checkpoint_id = l.content_checkpoint_id
                 WHERE l.link_state = 'unresolved'
                 ORDER BY l.created_at, l.content_checkpoint_id"
                    .to_string(),
            ))
            .await
            .map_err(|_| {
                CliError::fatal(
                    "agent doctor could not inspect unresolved subagent content links; check the repository database and rerun doctor"
                        .to_string(),
                )
            })?;
        for (ordinal, row) in unresolved.into_iter().enumerate() {
            let checkpoint_id: String = row.try_get_by("content_checkpoint_id").map_err(|_| {
                CliError::fatal(
                    "agent doctor could not decode an unresolved subagent content link".to_string(),
                )
            })?;
            findings.push(CheckpointFinding {
                inconsistency_type: CLASS_UNRESOLVED_SUBAGENT_LINK.to_string(),
                checkpoint_id: canonical_checkpoint_id(&checkpoint_id)
                    .unwrap_or_else(|| format!("unresolved-subagent-link-{}", ordinal + 1)),
                detail: "subagent content has no unique provider-stable boundary match; content and boundary evidence remain independently preserved, and automatic guessing is unsafe"
                    .to_string(),
                repaired: false,
                manual_required: true,
            });
            plans.push(RepairPlan::Manual);
        }
    }

    // Inspect raw marker rows so malformed state is surfaced instead of being
    // silently skipped. Live valid markers protect window A/B. Valid expired
    // markers and cleanup-pending markers are safely repairable through the
    // serialized root-fenced retirement. Cleanup-pending means the writer already
    // exited, so its ordinary TTL never delays repair. Malformed rows remain
    // manual-required because their OID ownership cannot be inferred from
    // untrusted JSON.
    let now_ms = Utc::now().timestamp_millis();
    let marker_entries = match crate::internal::metadata::MetadataKv::list_scope_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
    )
    .await
    {
        Ok(entries) => entries,
        Err(_) => {
            notes.push("in-flight marker listing unavailable".to_string());
            Vec::new()
        }
    };
    let mut markers = Vec::new();
    for (ordinal, entry) in marker_entries.into_iter().enumerate() {
        // Canonical checkpoint UUIDs are public ids (`checkpoint list/show`);
        // a damaged attempt id/key keeps the opaque per-report label.
        let diagnostic_id = format!("inflight-marker-{}", ordinal + 1);
        match traces::decode_and_validate_traces_inflight_marker(
            &entry.value,
            &entry.target,
            &entry.key,
        ) {
            Ok(marker) if !marker.time_fields_trustworthy(now_ms) => {
                // W2 §C.4.3: a future-dated start beyond skew tolerance is a
                // CORRUPT row — the prune-side listing fails closed on it,
                // blocking GC/prune/erasure until it is retired. Doctor must
                // therefore surface it as repairable (the same serialized
                // root-fenced retirement drains its listed OIDs safely),
                // never classify it as live.
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_EXPIRED_INFLIGHT_MARKER.to_string(),
                    checkpoint_id: canonical_checkpoint_id(&marker.attempt_id)
                        .unwrap_or(diagnostic_id),
                    detail: format!(
                        "future-dated traces marker (started_at_ms {} is beyond clock-skew tolerance) blocks destructive maintenance \
                         fail-closed; `libra agent doctor --repair` will run serialized \
                         root-fenced ownership retirement",
                        marker.started_at_ms
                    ),
                    repaired: false,
                    manual_required: false,
                });
                plans.push(RepairPlan::RepairExpiredInflightMarker {
                    session_id: marker.session_id,
                    attempt_id: marker.attempt_id,
                    observed_at_ms: now_ms,
                });
            }
            Ok(marker) if marker.is_live(now_ms) && !marker.cleanup_pending => markers.push(marker),
            Ok(marker) => {
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_EXPIRED_INFLIGHT_MARKER.to_string(),
                    checkpoint_id: canonical_checkpoint_id(&marker.attempt_id)
                        .unwrap_or(diagnostic_id),
                    detail: format!(
                        "expired or cleanup-pending traces marker owns {} candidate object(s) (cleanup_pending={}); `libra agent doctor --repair` will run serialized root-fenced ownership retirement; repository GC owns payload reachability and reclamation",
                        marker.oids.len(),
                        marker.cleanup_pending
                    ),
                    repaired: false,
                    manual_required: false,
                });
                plans.push(RepairPlan::RepairExpiredInflightMarker {
                    session_id: marker.session_id,
                    attempt_id: marker.attempt_id,
                    observed_at_ms: now_ms,
                });
            }
            Err(_) => {
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_INVALID_INFLIGHT_MARKER.to_string(),
                    checkpoint_id: canonical_checkpoint_id(&entry.key).unwrap_or(diagnostic_id),
                    detail: "malformed traces writer marker; automatic removal is unsafe because object ownership cannot be decoded".to_string(),
                    repaired: false,
                    manual_required: true,
                });
                plans.push(RepairPlan::Manual);
            }
        }
    }
    report.live_inflight_markers = markers.len();
    let mut inflight_ids: BTreeSet<String> = BTreeSet::new();
    let mut inflight_commits: BTreeSet<String> = BTreeSet::new();
    for marker in &markers {
        inflight_ids.insert(marker.attempt_id.clone());
        if let Some(commit) = &marker.commit {
            inflight_commits.insert(commit.clone());
        }
    }

    // Walk refs/libra/traces first-parent and attribute each checkpoint to
    // its introducing commit.
    let head = match history.resolve_history_head().await {
        Ok(head) => head,
        Err(_) => {
            notes.push("traces ref is unresolvable".to_string());
            None
        }
    };
    let (ref_map, has_noncanonical_checkpoint_id) =
        walk_traces_checkpoints(&reader, head, &mut notes);
    report.ref_reachable_checkpoints = ref_map.len();

    // Do not put a caller-shaped tree name into the reachability map: class-2
    // repair ultimately persists this key in `agent_checkpoint`. One fixed
    // manual finding makes the corruption actionable without reflecting the
    // malformed identity in human, JSON, or tracing output.
    if has_noncanonical_checkpoint_id {
        findings.push(CheckpointFinding {
            inconsistency_type: CLASS_MISSING_CATALOG_ROW.to_string(),
            checkpoint_id: "checkpoint-id-redacted".to_string(),
            detail: "invalid checkpoint identity in refs/libra/traces; manual repair required because automatic catalog reconstruction is unsafe".to_string(),
            repaired: false,
            manual_required: true,
        });
        plans.push(RepairPlan::Manual);
    }

    let rows = load_catalog_rows(conn).await?;
    report.catalog_rows = rows.len() as i64;
    let row_ids: BTreeSet<String> = rows.iter().map(|r| r.checkpoint_id.clone()).collect();

    // Legacy-v1 classification first — legacy checkpoints are exempt from
    // all checkpoint-object classes. Ref-reachable checkpoints classify via their
    // introducing commit's tree; catalog-only rows via their own tree_oid.
    let mut legacy_ids: BTreeSet<String> = BTreeSet::new();
    for (id, rc) in &ref_map {
        if rc.is_legacy_v1() {
            legacy_ids.insert(id.clone());
        }
    }
    for row in &rows {
        if !ref_map.contains_key(&row.checkpoint_id)
            && row_layout_is_legacy_v1(&reader, &row.tree_oid, &row.checkpoint_id)
        {
            legacy_ids.insert(row.checkpoint_id.clone());
        }
    }
    report.legacy_v1_checkpoints = legacy_ids.len();

    // Only rows with an UNRECOVERABLE class-1 finding (`missing_objects`)
    // are excluded from class 3 ("unrecoverable falls back to class-1
    // reporting"). An auto-repairable `stale_catalog_row` must NOT
    // suppress class 3: its ref-side objects are intact, and both repairs
    // must land in the same `--repair` run (otherwise cloud-sync
    // visibility would stay broken until a second invocation).
    // Any checkpoint that needs manual treatment is excluded from later
    // repairs as well (including deferred terminal-finalizer completion).
    // A corrupt object that fails index validation is not safe evidence that
    // its capture became durable.
    let mut manual_checkpoint_ids: BTreeSet<String> = BTreeSet::new();

    // ---- Class 1: catalog rows vs object store / ref truth -------------
    // Per-checkpoint E4 sweeps are kept for class 3 (the `present` list is
    // exactly the writer-enqueued object set minus the commit).
    let mut e4_sweeps: HashMap<String, E4Sweep> = HashMap::new();
    for row in &rows {
        if legacy_ids.contains(&row.checkpoint_id) {
            continue;
        }
        let commit_ok = reader.exists_str(&row.traces_commit);
        let tree_ok = reader.exists_str(&row.tree_oid);
        let meta_ok = reader.exists_str(&row.metadata_blob_oid);
        let rc = ref_map.get(&row.checkpoint_id);

        // Full E4 sidecar sweep (six-entry tree + E5 chunks + manifest
        // declarations) — against ref truth when available, else the
        // row's own tree.
        let sweep = match rc {
            Some(rc) => sweep_e4_checkpoint_objects(&reader, &rc.root_tree, &row.checkpoint_id),
            None if tree_ok => {
                sweep_e4_checkpoint_objects(&reader, &row.tree_oid, &row.checkpoint_id)
            }
            None => E4Sweep::default(),
        };

        // Which objects count as "missing" depends on where truth lives:
        // - Ref-reachable rows: the ref-side sweep governs. It already
        //   covers the root tree, metadata.json, and every sidecar, and
        //   the introducing commit was readable during the walk. Corrupt
        //   row COLUMNS pointing at nonexistent OIDs are the stale-repair
        //   branch's business (the repair replaces exactly those values),
        //   not lost objects.
        // - Catalog-only rows: the row's columns are the only truth, so
        //   probe them directly and extend with the row-tree sweep
        //   (dropping sweep lines whose OID a column line already names).
        let missing_parts: Vec<String> = if rc.is_some() {
            sweep.missing.clone()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if !commit_ok {
                parts.push(format!(
                    "traces_commit {}",
                    diagnostic_oid(&row.traces_commit)
                ));
            }
            if !tree_ok {
                parts.push(format!("tree_oid {}", diagnostic_oid(&row.tree_oid)));
            }
            if !meta_ok {
                parts.push(format!(
                    "metadata_blob_oid {}",
                    diagnostic_oid(&row.metadata_blob_oid)
                ));
            }
            for part in &sweep.missing {
                let dup = (!commit_ok && part.contains(&row.traces_commit))
                    || (!tree_ok && part.contains(&row.tree_oid))
                    || (!meta_ok && part.contains(&row.metadata_blob_oid));
                if !dup {
                    parts.push(part.clone());
                }
            }
            parts
        };

        if let Some(rc) = rc {
            let expected_meta = rc.metadata_blob.as_deref();
            let stale = row.traces_commit != rc.commit
                || row.tree_oid != rc.root_tree
                || expected_meta.is_some_and(|meta| row.metadata_blob_oid != meta);
            // The ref-derived replacement values must themselves verify
            // before we call the row auto-repairable (the commit and root
            // tree were read during the walk; the metadata blob still
            // needs an existence probe).
            let replacement_ok = expected_meta.is_some_and(|meta| reader.exists_str(meta));
            if stale && replacement_ok {
                // Auto-repairable: deliberately NOT in manual_checkpoint_ids —
                // the same run's class-3 pass still checks/repairs this
                // checkpoint's (ref-side) object_index rows.
                // INVARIANT: replacement_ok proved expected_meta is Some.
                let metadata_blob_oid = expected_meta.unwrap_or_default().to_string();
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_STALE_CATALOG_ROW.to_string(),
                    checkpoint_id: diagnostic_checkpoint_id(&row.checkpoint_id),
                    detail: format!(
                        "row OIDs disagree with refs/libra/traces (ref: commit {}, tree {}); \
                         rebuild from ref",
                        rc.commit, rc.root_tree
                    ),
                    repaired: false,
                    manual_required: false,
                });
                plans.push(RepairPlan::UpdateCatalogRow {
                    checkpoint_id: row.checkpoint_id.clone(),
                    tree_oid: rc.root_tree.clone(),
                    metadata_blob_oid,
                    traces_commit: rc.commit.clone(),
                });
            }
            // Missing objects are reported independently of staleness —
            // rebuilding row columns cannot resurrect a lost sidecar blob.
            if !missing_parts.is_empty() || (stale && !replacement_ok) {
                manual_checkpoint_ids.insert(row.checkpoint_id.clone());
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_MISSING_OBJECTS.to_string(),
                    checkpoint_id: diagnostic_checkpoint_id(&row.checkpoint_id),
                    detail: missing_objects_detail(&missing_parts),
                    repaired: false,
                    manual_required: true,
                });
                plans.push(RepairPlan::Manual);
            }
        } else if !missing_parts.is_empty() {
            manual_checkpoint_ids.insert(row.checkpoint_id.clone());
            findings.push(CheckpointFinding {
                inconsistency_type: CLASS_MISSING_OBJECTS.to_string(),
                checkpoint_id: diagnostic_checkpoint_id(&row.checkpoint_id),
                detail: format!(
                    "{} (checkpoint is not reachable from refs/libra/traces, so it \
                     cannot be rebuilt automatically)",
                    missing_objects_detail(&missing_parts)
                ),
                repaired: false,
                manual_required: true,
            });
            plans.push(RepairPlan::Manual);
        }
        e4_sweeps.insert(row.checkpoint_id.clone(), sweep);
    }

    // ---- Class 2: ref-reachable checkpoints without a catalog row ------
    for (id, rc) in &ref_map {
        if legacy_ids.contains(id) || row_ids.contains(id) {
            continue;
        }
        // Writers mid-flight (window B) are not inconsistencies.
        if inflight_ids.contains(id) || inflight_commits.contains(&rc.commit) {
            continue;
        }
        // A row may exist under a different checkpoint_id for the same
        // commit (e.g. an earlier repair raced a crash retry) — the same
        // probe the writer uses keeps this idempotent.
        match traces::agent_checkpoint_id_for_traces_commit(conn, &rc.commit).await {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(_) => {
                return Err(CliError::fatal(
                    "agent doctor could not inspect checkpoint catalog state; check the repository database and rerun doctor"
                        .to_string(),
                ));
            }
        }
        let (detail, plan) = build_class2_plan(conn, &reader, id, rc).await?;
        let manual = matches!(plan, RepairPlan::Manual);
        findings.push(CheckpointFinding {
            inconsistency_type: CLASS_MISSING_CATALOG_ROW.to_string(),
            checkpoint_id: diagnostic_checkpoint_id(id),
            detail,
            repaired: false,
            manual_required: manual,
        });
        plans.push(plan);
    }

    // ---- Class 3: checkpoint objects missing from object_index ---------
    let repo_id = resolve_repo_id(conn).await;
    let mut validation_budget = ObjectIndexValidationBudget::new();
    for row in &rows {
        if legacy_ids.contains(&row.checkpoint_id)
            || manual_checkpoint_ids.contains(&row.checkpoint_id)
        {
            continue;
        }
        // Full writer-enqueued object set: the traces commit plus every
        // object the E4 sweep verified (all trees, sidecar blobs, and
        // transcript chunks) with the writer's o_type tags. Truth is
        // ref-side when available: for a stale row (repaired in this same
        // run) the row columns are corrupt, so the commit target must be
        // the ref's introducing commit, and the sweep already ran on the
        // ref-side root.
        let commit_truth = ref_map
            .get(&row.checkpoint_id)
            .map(|rc| rc.commit.clone())
            .unwrap_or_else(|| row.traces_commit.clone());
        let mut targets: Vec<(String, &'static str, String)> = vec![(
            commit_truth.clone(),
            "commit",
            format!("traces_commit {}", diagnostic_oid(&commit_truth)),
        )];
        if let Some(sweep) = e4_sweeps.get(&row.checkpoint_id) {
            for object in &sweep.present {
                targets.push((
                    object.oid.clone(),
                    object.o_type,
                    format!("{} {}", object.label, diagnostic_oid(&object.oid)),
                ));
            }
        }
        let mut missing: Vec<(String, String, i64)> = Vec::new();
        let mut missing_names: Vec<String> = Vec::new();
        let mut drifted: Vec<(String, String, i64)> = Vec::new();
        let mut drifted_names: Vec<String> = Vec::new();
        let mut seen_oids: BTreeSet<String> = BTreeSet::new();
        let mut validation_failed = false;
        for (oid, o_type, label) in targets {
            if !seen_oids.insert(oid.clone()) {
                continue;
            }
            // Never repair an index row from manifest-provided `byte_len`.
            // Instead stream-hash the descriptor-pinned loose object in a
            // fixed buffer and use its verified header size. This validates
            // the actual object without retaining transcript bytes.
            let size = match crate::internal::object_format::parse_repo_oid(&oid)
                .map_err(|_| anyhow::anyhow!("invalid object identifier"))
                .and_then(|hash| {
                    validation_budget.validate(&reader, &hash, expected_git_object_type(o_type))
                }) {
                Ok(size) => size,
                Err(_) => {
                    validation_failed = true;
                    break;
                }
            };
            if let Some((existing_type, existing_size)) =
                object_index_row_shape(conn, &oid, &repo_id).await?
            {
                // A row that exists but drifted from the writer's
                // semantics (e.g. a transcript blob indexed as a generic
                // `blob`, or a wrong size) breaks cloud-sync classification
                // just like a missing row. Size is proven from the
                // descriptor-pinned content-addressed object stream.
                let type_drift = existing_type != o_type;
                let size_drift = size != existing_size;
                if type_drift || size_drift {
                    drifted.push((oid.clone(), o_type.to_string(), size));
                    drifted_names.push(format!(
                        "{label} (was {}/{existing_size})",
                        diagnostic_object_index_type(&existing_type)
                    ));
                }
                continue;
            }
            missing.push((oid.clone(), o_type.to_string(), size));
            missing_names.push(label);
        }
        if validation_failed {
            // A loose object can disappear, be malformed, or exceed the
            // aggregate validation budget after the E4 sweep proved only its
            // name. Treat the entire checkpoint as manual: a partial index
            // plan would make cloud sync observe an incomplete capture.
            manual_checkpoint_ids.insert(row.checkpoint_id.clone());
            findings.push(CheckpointFinding {
                inconsistency_type: CLASS_MISSING_OBJECT_INDEX.to_string(),
                checkpoint_id: diagnostic_checkpoint_id(&row.checkpoint_id),
                detail: "checkpoint object integrity could not be validated within doctor safety limits; no object-index rows were changed and manual review is required".to_string(),
                repaired: false,
                manual_required: true,
            });
            plans.push(RepairPlan::Manual);
            continue;
        }
        if missing.is_empty() && drifted.is_empty() {
            continue;
        }
        let mut detail_parts: Vec<String> = Vec::new();
        if !missing_names.is_empty() {
            detail_parts.push(format!(
                "object_index rows missing for {}",
                missing_names.join(", ")
            ));
        }
        if !drifted_names.is_empty() {
            detail_parts.push(format!(
                "object_index rows drifted from writer semantics for {}",
                drifted_names.join(", ")
            ));
        }
        findings.push(CheckpointFinding {
            inconsistency_type: CLASS_MISSING_OBJECT_INDEX.to_string(),
            checkpoint_id: diagnostic_checkpoint_id(&row.checkpoint_id),
            detail: format!(
                "{} (objects would not reach `libra cloud sync` correctly)",
                detail_parts.join("; ")
            ),
            repaired: false,
            manual_required: false,
        });
        plans.push(RepairPlan::InsertObjectIndex {
            entries: missing,
            updates: drifted,
        });
    }

    // ---- ACF-07: terminal receipts left after durable checkpoint --------
    // A crash after the ref/catalog checkpoint transaction but before strict
    // receipt completion has no live marker to protect it. Reuse the existing
    // checkpoint-store `expired_inflight_marker` classification rather than
    // changing doctor JSON: the receipt is an expired finalization attempt
    // whose deterministic checkpoint is already ref-reachable. The catalog
    // owns the subsequent marker/source/revision revalidation.
    match CaptureCatalogStore::new(conn.clone())
        .pending_finalizer_recoveries_for_doctor(now_ms)
        .await
    {
        Ok(scan) => {
            if scan.malformed_rows != 0 {
                notes.push("one or more pending terminal-finalizer rows are malformed or oversized; those rows require manual catalog recovery and were not repaired".to_string());
            }
            if scan.skipped_invalid_keys {
                notes.push("pending terminal-finalizer scan skipped malformed session keys; diagnostic coverage is incomplete and skipped rows require manual catalog recovery".to_string());
            }
            if scan.truncated {
                notes.push("pending terminal-finalizer scan reached its bounded limit; this report is incomplete and unlisted receipts were not repaired".to_string());
            }
            for recovery in scan.recoveries {
                let checkpoint_id = recovery.checkpoint_id().to_string();
                // A live marker means the writer is still protected by its
                // normal window-A/B protocol; doctor must not race it.
                if !recovery.manual_only() && inflight_ids.contains(&checkpoint_id) {
                    continue;
                }
                let durable = ref_map.contains_key(&checkpoint_id)
                    && !manual_checkpoint_ids.contains(&checkpoint_id);
                let budget_exhausted = recovery.budget_exhausted();
                let manual_only = recovery.manual_only();
                let artifact_present = recovery.artifact_present();
                let artifact_pending = recovery.artifact_pending();
                let artifact_manual_attempted = recovery.artifact_manual_attempted();
                let (detail, repair_kind) = diagnose_pending_finalizer(
                    manual_only,
                    durable,
                    budget_exhausted,
                    artifact_present,
                    artifact_pending,
                    artifact_manual_attempted,
                );
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_EXPIRED_INFLIGHT_MARKER.to_string(),
                    checkpoint_id: diagnostic_checkpoint_id(&checkpoint_id),
                    detail: detail.to_string(),
                    repaired: false,
                    manual_required: manual_only || !durable,
                });
                plans.push(match repair_kind {
                    FinalizerRepairKind::Manual => RepairPlan::Manual,
                    FinalizerRepairKind::CompleteDurable => RepairPlan::RecoverPendingFinalizer {
                        recovery,
                        observed_at_ms: now_ms,
                    },
                    FinalizerRepairKind::QuarantineExhausted => {
                        RepairPlan::QuarantineExhaustedPendingFinalizer {
                            recovery,
                            observed_at_ms: now_ms,
                        }
                    }
                    FinalizerRepairKind::QuarantineStaleArtifact => {
                        RepairPlan::QuarantineStalePendingArtifact { recovery }
                    }
                    FinalizerRepairKind::ReplayArtifact => RepairPlan::ReplayPendingArtifact {
                        recovery,
                        observed_at_ms: now_ms,
                    },
                });
            }
        }
        Err(_) => notes.push(
            "pending terminal-finalizer receipt scan unavailable; no terminal state was advanced"
                .to_string(),
        ),
    }

    // ---- Repair execution (idempotent; spans per attempt) ---------------
    if repair {
        let mut replay_batch = ArtifactReplayBatch::new();
        for (finding, plan) in findings.iter_mut().zip(plans.iter()) {
            execute_repair(conn, &history, finding, plan, &repo_id, &mut replay_batch).await;
            emit_repair_span(finding);
        }
    }

    report.repaired = findings.iter().filter(|f| f.repaired).count();
    report.manual_required = findings.iter().filter(|f| f.manual_required).count();
    report.findings = findings;
    if !notes.is_empty() {
        report.note = Some(notes.join("; "));
    }
    Ok(report)
}

/// Exercise the production doctor classification-and-repair orchestrator from
/// in-crate recovery fixtures without making its internal report a public API.
#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))]
pub(crate) async fn scan_checkpoint_store_for_test(
    conn: &DatabaseConnection,
    schema_present: bool,
    repair: bool,
) -> CliResult<serde_json::Value> {
    let report = scan_checkpoint_store(conn, schema_present, repair).await?;
    serde_json::to_value(report).map_err(|_| {
        CliError::fatal("could not serialize the in-process doctor test report".to_string())
    })
}

/// Same orchestrator with a widened cooperative replay budget, for fixtures
/// that assert repair semantics rather than timing under full-suite load.
#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))]
pub(crate) async fn scan_checkpoint_store_with_replay_budget_for_test(
    conn: &DatabaseConnection,
    schema_present: bool,
    repair: bool,
    budget: Duration,
) -> CliResult<serde_json::Value> {
    TEST_ARTIFACT_REPLAY_BUDGET
        .scope(
            budget,
            scan_checkpoint_store_for_test(conn, schema_present, repair),
        )
        .await
}

/// One `agent.doctor.repair` span per repair attempt (`agent.md` §6).
/// Required fields: `inconsistency_type`, `repaired`, `manual_required`.
/// Forbidden: raw transcript, metadata, provider identity, and caller-shaped
/// checkpoint IDs. The stable inconsistency class is sufficient correlation.
fn emit_repair_span(finding: &CheckpointFinding) {
    let span = tracing::info_span!(
        "agent.doctor.repair",
        inconsistency_type = %finding.inconsistency_type,
        repaired = finding.repaired,
        manual_required = finding.manual_required,
    );
    let _guard = span.enter();
    tracing::info!("agent doctor repair attempt");
}

/// Cooperative budget for one doctor replay batch (and for a fresh one-shot
/// manual attempt). It bounds cancellable work, not arbitrary syscalls.
const ARTIFACT_REPLAY_BUDGET: Duration = Duration::from_secs(2);

#[cfg(test)]
tokio::task_local! {
    static TEST_ARTIFACT_REPLAY_BUDGET: Duration;
}

fn replay_budget() -> Duration {
    #[cfg(test)]
    if let Ok(budget) = TEST_ARTIFACT_REPLAY_BUDGET.try_with(|budget| *budget) {
        return budget;
    }
    ARTIFACT_REPLAY_BUDGET
}

/// One doctor invocation shares a single bounded replay budget across every
/// finding. Re-querying the indexed queue for each finding must not let a
/// successful removal pull a sixth receipt into the same repair batch.
struct ArtifactReplayBatch {
    attempts_remaining: u8,
    deadline: Option<CaptureCommitDeadline>,
}

impl ArtifactReplayBatch {
    const MAX_ATTEMPTS: u8 = 5;

    fn new() -> Self {
        Self {
            attempts_remaining: Self::MAX_ATTEMPTS,
            deadline: None,
        }
    }

    fn deadline(&mut self) -> Result<CaptureCommitDeadline> {
        if self.deadline.is_none() {
            self.deadline = Some(CaptureCommitDeadline::from_budget(replay_budget())?);
        }
        self.deadline
            .context("capture recovery batch deadline was not established")
    }

    fn attempt_deadline(&mut self, manual: bool) -> Result<CaptureCommitDeadline> {
        if manual {
            CaptureCommitDeadline::from_budget(replay_budget())
        } else {
            self.deadline()
        }
    }

    fn has_capacity(&self) -> bool {
        self.attempts_remaining > 0
    }

    fn record_attempt(&mut self) {
        self.attempts_remaining = self.attempts_remaining.saturating_sub(1);
    }
}

async fn replay_capture_artifact(
    conn: &DatabaseConnection,
    finding: &mut CheckpointFinding,
    recovery: &CaptureCatalogFinalizerRecovery,
    observed_at_ms: i64,
    manual: bool,
    replay_batch: &mut ArtifactReplayBatch,
) -> ArtifactReplayDisposition {
    if !replay_batch.has_capacity() {
        finding.manual_required = true;
        finding.detail.push_str(
            "; bounded recovery batch limit was reached; rerun `libra agent doctor --repair` to continue",
        );
        return ArtifactReplayDisposition::Deferred;
    }
    let storage = util::request_storage_path();
    let root = util::request_working_dir();
    let repo_path = match util::request_worktree_gitdir() {
        Ok(path) => path,
        Err(_) => {
            finding.manual_required = true;
            finding.detail.push_str(
                "; repository checkpoint storage could not be resolved; no checkpoint was written",
            );
            return ArtifactReplayDisposition::Refused;
        }
    };
    let deadline = match replay_batch.attempt_deadline(manual) {
        Ok(deadline) => deadline,
        Err(_) => {
            finding.manual_required = true;
            finding.detail.push_str(
                "; bounded capture recovery deadline could not be established; no checkpoint was written",
            );
            return ArtifactReplayDisposition::Refused;
        }
    };
    let replay_result = recovery::replay_for_doctor(recovery::DoctorReplayRequest {
        conn,
        recovery,
        storage: &storage,
        root: &root,
        repo_path: &repo_path,
        now_millis: observed_at_ms,
        deadline,
        manual,
    })
    .await;
    let replay_result = match replay_result {
        Err(error) if pending::retryable_load_failure(&error, deadline.monotonic()) => {
            Ok(ArtifactRecoveryOutcome::RetryLater)
        }
        Err(error) if is_permanent_pending_replay_failure(&error) => {
            Ok(ArtifactRecoveryOutcome::ManualRequired)
        }
        result => result,
    };
    if !matches!(
        replay_result,
        Ok(ArtifactRecoveryOutcome::DeferredByBatchLimit | ArtifactRecoveryOutcome::RetryLater)
    ) {
        replay_batch.record_attempt();
    }
    match &replay_result {
        Ok(ArtifactRecoveryOutcome::Completed | ArtifactRecoveryOutcome::AlreadyComplete) => {
            finding.repaired = true;
            finding.manual_required = false;
            finding.detail.push_str(
                "; authenticated local checkpoint was durably published and its original terminal receipt completed",
            );
        }
        Ok(ArtifactRecoveryOutcome::DeferredByBatchLimit) => {
            finding.manual_required = true;
            finding.detail.push_str(
                "; bounded recovery batch limit was reached; rerun `libra agent doctor --repair` to continue",
            );
        }
        Ok(ArtifactRecoveryOutcome::RetryLater) => {
            finding.manual_required = true;
            finding.detail.push_str(
                "; another capture attempt is still active or cleanup is pending; automatic replay will retry later",
            );
        }
        Ok(ArtifactRecoveryOutcome::ManualRequired) | Err(_) => {
            finding.manual_required = true;
            finding.detail.push_str(
                "; authenticated artifact replay was refused or failed closed; manual recovery is required",
            );
        }
    }
    match &replay_result {
        Ok(ArtifactRecoveryOutcome::Completed | ArtifactRecoveryOutcome::AlreadyComplete) => {
            ArtifactReplayDisposition::Completed
        }
        Ok(ArtifactRecoveryOutcome::ManualRequired) => ArtifactReplayDisposition::PermanentFailure,
        Ok(ArtifactRecoveryOutcome::DeferredByBatchLimit) => ArtifactReplayDisposition::Deferred,
        Ok(ArtifactRecoveryOutcome::RetryLater) => ArtifactReplayDisposition::RetryLater,
        Err(_) => ArtifactReplayDisposition::Refused,
    }
}

fn is_permanent_pending_replay_failure(error: &anyhow::Error) -> bool {
    use crate::internal::ai::capture::catalog::CaptureCatalogError;

    error.chain().any(|cause| {
        cause
            .downcast_ref::<CaptureCatalogError>()
            .is_some_and(|error| {
                matches!(
                    error,
                    CaptureCatalogError::InvalidRequest
                        | CaptureCatalogError::InvalidReceiptKey
                        | CaptureCatalogError::MalformedReceiptLedger
                        | CaptureCatalogError::ReceiptCapacityExhausted
                        | CaptureCatalogError::ScopeRejected
                        | CaptureCatalogError::Tombstoned
                        | CaptureCatalogError::ImportSessionConflict
                )
            })
            || cause
                .to_string()
                .contains("retained capture identity is missing")
    })
}

/// Execute one repair plan, updating the finding in place. Repair failures
/// never abort the run — they annotate the finding and leave it
/// unrepaired so the operator sees exactly what happened.
async fn execute_repair(
    conn: &DatabaseConnection,
    history: &HistoryManager,
    finding: &mut CheckpointFinding,
    plan: &RepairPlan,
    repo_id: &str,
    replay_batch: &mut ArtifactReplayBatch,
) {
    let finalizer_recovery = match plan {
        RepairPlan::RecoverPendingFinalizer { recovery, .. }
        | RepairPlan::QuarantineExhaustedPendingFinalizer { recovery, .. }
        | RepairPlan::QuarantineStalePendingArtifact { recovery }
        | RepairPlan::ReplayPendingArtifact { recovery, .. } => Some(recovery),
        _ => None,
    };
    if let Some(recovery) = finalizer_recovery {
        let root = util::request_working_dir();
        match CaptureScope::resolve(conn, &root).await {
            Ok(scope) if &scope != recovery.scope() => {
                finding.detail.push_str(
                    "; retained terminal capture belongs to a different worktree/workspace scope and was not replayed or charged against this worktree's recovery budget",
                );
                return;
            }
            Ok(scope) => {
                if scope.assert_workspace_fence_live(conn).await.is_err() {
                    finding.detail.push_str(
                        "; current workspace lease could not be verified; repair was deferred without changing the artifact",
                    );
                    return;
                }
            }
            Err(_) => {
                finding.detail.push_str(
                    "; current capture scope could not be verified; repair was deferred without changing the artifact",
                );
                return;
            }
        }
    }
    match plan {
        RepairPlan::QuarantineExhaustedPendingFinalizer {
            recovery,
            observed_at_ms,
        } => match CaptureCatalogStore::new(conn.clone())
            .quarantine_exhausted_pending_finalizer(recovery, *observed_at_ms)
            .await
        {
            Ok(CaptureCatalogFinalizerRecoveryResult::Quarantined) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; exhausted artifact was retained in quarantine without changing its original receipt; explicit one-shot replay follows",
                );
                if recovery.artifact_present() {
                    let _ = replay_capture_artifact(
                        conn,
                        finding,
                        recovery,
                        *observed_at_ms,
                        true,
                        replay_batch,
                    )
                    .await;
                }
            }
            Ok(CaptureCatalogFinalizerRecoveryResult::AlreadyComplete) => {
                finding.repaired = true;
            }
            Ok(CaptureCatalogFinalizerRecoveryResult::Pending) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; original finalizer budget is not exhausted; it remains pending",
                );
            }
            Ok(
                CaptureCatalogFinalizerRecoveryResult::MissingDurableCheckpoint
                | CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged,
            ) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; finalizer fence changed during quarantine recovery; no terminal state was advanced",
                );
            }
            Ok(CaptureCatalogFinalizerRecoveryResult::Completed) => {
                // This path has no durable checkpoint by construction; keep
                // the outcome fail-closed if a future implementation returns
                // a terminal completion here.
                finding.manual_required = true;
                finding.detail.push_str(
                    "; unexpected terminal recovery result was rejected",
                );
            }
            Err(_) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; finalizer quarantine recovery failed closed",
                );
            }
        },
        RepairPlan::QuarantineStalePendingArtifact { recovery } => {
            match quarantine_stale_pending_artifact(
                conn,
                recovery.scope(),
                recovery.checkpoint_id(),
            )
            .await
            {
                Ok(()) => finding.detail.push_str("; stale pending header was moved to quarantine under the current workspace fence; no replay was attempted"),
                Err(_) => {
                    finding.manual_required = true;
                    finding.detail.push_str("; stale artifact quarantine failed closed; no repair was recorded");
                }
            }
        }
        RepairPlan::RecoverPendingFinalizer {
            recovery,
            observed_at_ms,
        } => match CaptureCatalogStore::new(conn.clone())
            .recover_pending_finalizer_after_durable_checkpoint(recovery, *observed_at_ms)
            .await
        {
            Ok(
                CaptureCatalogFinalizerRecoveryResult::Completed
                | CaptureCatalogFinalizerRecoveryResult::AlreadyComplete,
            ) => finding.repaired = true,
            Ok(CaptureCatalogFinalizerRecoveryResult::MissingDurableCheckpoint) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; durable checkpoint evidence disappeared before recovery; no terminal state was advanced",
                );
            }
            Ok(CaptureCatalogFinalizerRecoveryResult::Pending) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; finalizer receipt is still pending after revalidation; no terminal state was advanced",
                );
            }
            Ok(CaptureCatalogFinalizerRecoveryResult::Quarantined) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; finalizer is already quarantined and requires operator recovery",
                );
            }
            Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; finalizer fence changed during doctor recovery; no terminal state was advanced",
                );
            }
            Err(_) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; finalizer recovery failed closed; no terminal state was advanced",
                );
            }
        },
        RepairPlan::ReplayPendingArtifact {
            recovery,
            observed_at_ms,
        } => {
            let replay_disposition = replay_capture_artifact(
                conn,
                finding,
                recovery,
                *observed_at_ms,
                false,
                replay_batch,
            )
            .await;
            if replay_disposition == ArtifactReplayDisposition::PermanentFailure
                && let Ok(scope) = CaptureScope::resolve(conn, &util::request_working_dir()).await
                && &scope == recovery.scope()
                && quarantine_worker_candidate(
                    conn,
                    &scope,
                    recovery.checkpoint_id(),
                    replay_batch.deadline().ok().map(CaptureCommitDeadline::monotonic),
                )
                .await
            {
                finding.detail.push_str(
                    "; failed automatic candidate was retained in quarantine for operator inspection",
                );
            }
        }
        RepairPlan::Manual => {}
        RepairPlan::RepairExpiredInflightMarker {
            session_id,
            attempt_id,
            observed_at_ms,
        } => match history
            .repair_expired_traces_inflight_marker(session_id, attempt_id, *observed_at_ms)
            .await
        {
            Ok(true) => finding.repaired = true,
            Ok(false) => finding.detail.push_str(
                "; repair skipped because the marker was refreshed or remained protected; retry after the active writer exits",
            ),
            Err(_) => {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; reachability repair failed closed; retry after active writers finish or rerun doctor",
                );
            }
        },
        RepairPlan::InsertCatalogRow {
            checkpoint_id,
            session_id,
            parent_commit,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
            created_at,
        } => {
            let row = AgentCheckpointRow {
                checkpoint_id,
                session_id,
                parent_commit: parent_commit.as_deref(),
                tree_oid,
                metadata_blob_oid,
                traces_commit,
                created_at: *created_at,
            };
            match repair_insert_agent_checkpoint_catalog_row(conn, &row).await {
                // `false` means another writer (or an earlier repair)
                // already landed the row — the inconsistency is gone
                // either way.
                Ok(_) => finding.repaired = true,
                Err(_) => {
                    mark_catalog_repair_failed(finding);
                }
            }
        }
        RepairPlan::InsertSubagentCatalogRow {
            checkpoint_id,
            session_id,
            parent_commit,
            parent_checkpoint_id,
            subagent_session_id,
            tool_use_id,
            description,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
            created_at,
        } => {
            let row = SubagentCheckpointRow {
                checkpoint_id,
                session_id,
                parent_commit: parent_commit.as_deref(),
                parent_checkpoint_id: parent_checkpoint_id.as_deref(),
                subagent_session_id: subagent_session_id.as_deref(),
                tool_use_id: tool_use_id.as_deref(),
                description: description.as_deref(),
                tree_oid,
                metadata_blob_oid,
                traces_commit,
                created_at: *created_at,
            };
            match repair_insert_subagent_checkpoint_catalog_row(conn, &row).await {
                Ok(_) => finding.repaired = true,
                Err(_) => {
                    mark_catalog_repair_failed(finding);
                }
            }
        }
        RepairPlan::UpdateCatalogRow {
            checkpoint_id,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
        } => {
            match repair_update_checkpoint_catalog_row(
                conn,
                checkpoint_id,
                tree_oid,
                metadata_blob_oid,
                traces_commit,
            )
            .await
            {
                Ok(true) => finding.repaired = true,
                Ok(false) => {
                    finding.manual_required = true;
                    finding.detail.push_str(
                        "; checkpoint row disappeared before its scoped repair could commit; rerun doctor",
                    );
                }
                Err(_) => {
                    mark_catalog_repair_failed(finding);
                }
            }
        }
        RepairPlan::InsertObjectIndex { entries, updates } => {
            if repair_object_index_rows_atomically(conn, entries, updates, repo_id)
                .await
                .is_ok()
            {
                finding.repaired = true;
            } else {
                finding.manual_required = true;
                finding.detail.push_str(
                    "; object-index repair failed closed; rerun doctor after resolving the store condition",
                );
            }
        }
    }
}

/// Make a failed scoped catalog repair visibly actionable.  In particular, a
/// workspace fence loss must never leave the finding looking like a harmless
/// best-effort miss: the operator needs to rerun from the current workspace
/// or resolve the ownership record before doctor can mutate it.
fn mark_catalog_repair_failed(finding: &mut CheckpointFinding) {
    finding.manual_required = true;
    finding.detail.push_str(
        "; scoped catalog repair failed closed; rerun doctor from the current workspace after resolving the ownership record",
    );
}

/// Load the durable owner for a session while holding the SQLite writer
/// transaction used by the repair.  Do not derive this from the current
/// process worktree: doctor may be invoked from a different worktree, and
/// the existing scoped session is the authority for a catalog backfill.
async fn doctor_catalog_repair_scope_for_session(
    txn: &DatabaseTransaction,
    session_id: &str,
) -> Result<DoctorCatalogRepairScope> {
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT scope_state, repo_id, worktree_id, workspace_id, workspace_fence
             FROM agent_session WHERE session_id = ? LIMIT 1",
            [session_id.into()],
        ))
        .await
        .context("read durable agent-session ownership for doctor catalog repair")?
        .with_context(|| {
            format!(
                "agent_session '{session_id}' disappeared before doctor could repair its checkpoint catalog"
            )
        })?;
    let scope_state: String = row
        .try_get_by("scope_state")
        .context("decode agent-session scope state for doctor catalog repair")?;
    match scope_state.as_str() {
        // Legacy captures intentionally have no attributable workspace
        // fence. Keep their historical repair contract explicit rather than
        // silently treating a malformed scoped row as unscoped.
        "legacy_unknown" => Ok(DoctorCatalogRepairScope::LegacyUnscoped),
        "scoped" => {
            let repo_id: Option<String> = row
                .try_get_by("repo_id")
                .context("decode agent-session repository ownership for doctor catalog repair")?;
            let worktree_id: Option<String> = row
                .try_get_by("worktree_id")
                .context("decode agent-session worktree ownership for doctor catalog repair")?;
            let workspace_id: Option<String> = row
                .try_get_by("workspace_id")
                .context("decode agent-session workspace ownership for doctor catalog repair")?;
            let workspace_fence: Option<i64> = row
                .try_get_by("workspace_fence")
                .context("decode agent-session workspace fence for doctor catalog repair")?;
            let repo_id = repo_id
                .filter(|value| !value.is_empty())
                .context("scoped agent session has no repository owner; inspect it with `libra worktree doctor`")?;
            let worktree_id = worktree_id.context(
                "scoped agent session has no worktree owner; inspect it with `libra worktree doctor`",
            )?;
            match (&workspace_id, workspace_fence) {
                (None, None) => {}
                (Some(id), Some(_)) if !id.is_empty() => {}
                _ => {
                    bail!(
                        "scoped agent session has an incomplete workspace owner; inspect it with `libra worktree doctor`"
                    );
                }
            }
            Ok(DoctorCatalogRepairScope::Scoped(CaptureScope {
                repo_id,
                worktree_id,
                workspace_id,
                workspace_fence,
            }))
        }
        other => bail!(
            "agent session has unknown scope state '{other}'; inspect it with `libra worktree doctor` before repairing its checkpoint catalog"
        ),
    }
}

async fn assert_doctor_catalog_repair_scope_live(
    txn: &DatabaseTransaction,
    scope: &DoctorCatalogRepairScope,
    operation: &'static str,
) -> Result<()> {
    if let DoctorCatalogRepairScope::Scoped(scope) = scope {
        scope
            .assert_workspace_fence_live(txn)
            .await
            .with_context(|| format!("verify capture workspace lease before {operation}"))?;
    }
    Ok(())
}

async fn begin_doctor_catalog_repair_transaction(
    conn: &DatabaseConnection,
    session_id: &str,
    operation: &'static str,
) -> Result<(DatabaseTransaction, DoctorCatalogRepairScope)> {
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .with_context(|| format!("begin {operation}"))?;
    let scope = match doctor_catalog_repair_scope_for_session(&txn, session_id).await {
        Ok(scope) => scope,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error).with_context(|| format!("resolve durable scope before {operation}"));
        }
    };
    if let Err(error) = assert_doctor_catalog_repair_scope_live(&txn, &scope, operation).await {
        txn.rollback().await.ok();
        return Err(error);
    }
    Ok((txn, scope))
}

/// Commit a doctor catalog mutation. Scoped repairs put the conditional lease
/// DML last so an expiry after the INSERT/UPDATE rolls the entire repair back.
async fn commit_doctor_catalog_repair_transaction(
    txn: DatabaseTransaction,
    scope: &DoctorCatalogRepairScope,
    operation: &'static str,
    changed_catalog: bool,
) -> Result<()> {
    if changed_catalog
        && let DoctorCatalogRepairScope::Scoped(scope) = scope
        && let Err(error) = scope.assert_workspace_fence_live_for_commit(&txn).await
    {
        txn.rollback().await.ok();
        return Err(error).with_context(|| {
            format!("verify capture workspace lease before committing {operation}")
        });
    }
    txn.commit()
        .await
        .with_context(|| format!("commit {operation}"))
}

async fn repair_insert_agent_checkpoint_catalog_row(
    conn: &DatabaseConnection,
    row: &AgentCheckpointRow<'_>,
) -> Result<bool> {
    const OPERATION: &str = "doctor missing checkpoint catalog-row repair";
    let (txn, scope) =
        begin_doctor_catalog_repair_transaction(conn, row.session_id, OPERATION).await?;
    let inserted = match insert_agent_checkpoint_row_idempotent(&txn, row).await {
        Ok(inserted) => inserted,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error).context("insert missing checkpoint catalog row");
        }
    };
    commit_doctor_catalog_repair_transaction(txn, &scope, OPERATION, inserted).await?;
    Ok(inserted)
}

async fn repair_insert_subagent_checkpoint_catalog_row(
    conn: &DatabaseConnection,
    row: &SubagentCheckpointRow<'_>,
) -> Result<bool> {
    const OPERATION: &str = "doctor missing subagent checkpoint catalog-row repair";
    let (txn, scope) =
        begin_doctor_catalog_repair_transaction(conn, row.session_id, OPERATION).await?;
    let inserted = match insert_subagent_checkpoint_row_idempotent(&txn, row).await {
        Ok(inserted) => inserted,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error).context("insert missing subagent checkpoint catalog row");
        }
    };
    commit_doctor_catalog_repair_transaction(txn, &scope, OPERATION, inserted).await?;
    Ok(inserted)
}

async fn repair_update_checkpoint_catalog_row(
    conn: &DatabaseConnection,
    checkpoint_id: &str,
    tree_oid: &str,
    metadata_blob_oid: &str,
    traces_commit: &str,
) -> Result<bool> {
    const OPERATION: &str = "doctor stale checkpoint catalog-row repair";
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .with_context(|| format!("begin {OPERATION}"))?;
    let row = match txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT session_id FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [checkpoint_id.into()],
        ))
        .await
        .context("read checkpoint session ownership for doctor catalog repair")?
    {
        Some(row) => row,
        None => {
            txn.rollback().await.ok();
            return Ok(false);
        }
    };
    let session_id: String = match row.try_get_by("session_id") {
        Ok(session_id) => session_id,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error)
                .context("decode checkpoint session ownership for doctor catalog repair");
        }
    };
    let scope = match doctor_catalog_repair_scope_for_session(&txn, &session_id).await {
        Ok(scope) => scope,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error).with_context(|| format!("resolve durable scope before {OPERATION}"));
        }
    };
    if let Err(error) = assert_doctor_catalog_repair_scope_live(&txn, &scope, OPERATION).await {
        txn.rollback().await.ok();
        return Err(error);
    }
    let updated = match txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_checkpoint
             SET tree_oid = ?, metadata_blob_oid = ?, traces_commit = ?,
                 sync_revision = sync_revision + 1
             WHERE checkpoint_id = ? AND session_id = ?",
            [
                tree_oid.into(),
                metadata_blob_oid.into(),
                traces_commit.into(),
                checkpoint_id.into(),
                session_id.into(),
            ],
        ))
        .await
    {
        Ok(result) => result.rows_affected() == 1,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error).context("update stale checkpoint catalog row");
        }
    };
    commit_doctor_catalog_repair_transaction(txn, &scope, OPERATION, updated).await?;
    Ok(updated)
}

/// Class-2 repair plan: reconstruct the catalog row from the introducing
/// commit's `metadata.json` (v1 and v2 shapes both parse) and `Libra-*`
/// trailers. Anything unparsable / unsatisfiable degrades to `Manual` with
/// the reason in the detail string (never transcript content).
async fn build_class2_plan(
    conn: &DatabaseConnection,
    reader: &ObjectReader,
    checkpoint_id: &str,
    rc: &RefCheckpoint,
) -> CliResult<(String, RepairPlan)> {
    let base = format!(
        "commit {} on refs/libra/traces has no agent_checkpoint row (crash window B)",
        rc.commit
    );
    let Some(metadata_blob) = rc.metadata_blob.as_deref() else {
        return Ok((
            format!("{base}; metadata.json missing from the checkpoint tree — manual review"),
            RepairPlan::Manual,
        ));
    };
    let metadata_bytes = match crate::internal::object_format::parse_repo_oid(metadata_blob)
        .map_err(|e| anyhow::anyhow!("invalid metadata blob OID '{metadata_blob}': {e}"))
        .and_then(|hash| reader.read_raw(&hash))
    {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok((
                format!("{base}; metadata.json unreadable — manual review"),
                RepairPlan::Manual,
            ));
        }
    };
    let metadata: CheckpointMetadataProbe = match serde_json::from_slice(&metadata_bytes) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Ok((
                format!("{base}; metadata.json unparsable — manual review"),
                RepairPlan::Manual,
            ));
        }
    };
    // Class-2 rows come from either the committed writer (scope='committed')
    // or, since A0-02, the subagent writer (scope='subagent'); both stamp
    // `metadata.json` with the fields needed to rebuild their catalog row.
    // Any other scope is unexpected enough that a human should look.
    let scope = metadata
        .scope
        .clone()
        .or_else(|| rc.scope_trailer.clone())
        .unwrap_or_else(|| "committed".to_string());
    if metadata
        .subagent
        .as_ref()
        .and_then(|subagent| subagent.provenance.as_deref())
        == Some("content")
    {
        return Ok((
            format!(
                "{base}; content checkpoint requires claim/revision/link recovery and cannot be repaired as a boundary-only catalog row — manual review"
            ),
            RepairPlan::Manual,
        ));
    }
    // Shared classification boundary (plan-20260713 DR-05c-0): doctor and
    // claim recovery both rebuild catalog rows through
    // `rebuild_catalog_row_from_traces_ref`, so an unknown scope fails
    // closed identically everywhere.
    let rebuilt = traces::rebuild_catalog_row_from_traces_ref(traces::RebuildCatalogRowInputs {
        scope: scope.clone(),
        checkpoint_id: checkpoint_id.to_string(),
        session_id: metadata.session_id.clone(),
        parent_commit: rc.parent_commit_trailer.clone(),
        parent_checkpoint_id: metadata.parent_checkpoint_id.clone(),
        subagent_session_id: metadata.subagent_session_id.clone(),
        tool_use_id: metadata.tool_use_id.clone(),
        description: metadata.description.clone(),
        tree_oid: rc.root_tree.clone(),
        metadata_blob_oid: metadata_blob.to_string(),
        traces_commit: rc.commit.clone(),
        created_at: metadata.created_at,
    });
    let rebuilt = match rebuilt {
        Ok(rebuilt) => rebuilt,
        Err(_) => {
            return Ok((
                format!("{base}; checkpoint scope is not auto-repairable — manual review"),
                RepairPlan::Manual,
            ));
        }
    };
    if !session_exists(conn, &metadata.session_id).await? {
        return Ok((
            format!("{base}; required agent-session record is missing — manual review"),
            RepairPlan::Manual,
        ));
    }
    match rebuilt {
        traces::RebuiltCatalogRow::Subagent {
            checkpoint_id,
            session_id,
            parent_commit,
            parent_checkpoint_id,
            subagent_session_id,
            tool_use_id,
            description,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
            created_at,
        } => Ok((
            format!("{base}; scope='subagent' row can be rebuilt from the commit's metadata.json"),
            RepairPlan::InsertSubagentCatalogRow {
                checkpoint_id,
                session_id,
                parent_commit,
                parent_checkpoint_id,
                subagent_session_id,
                tool_use_id,
                description,
                tree_oid,
                metadata_blob_oid,
                traces_commit,
                created_at,
            },
        )),
        traces::RebuiltCatalogRow::Committed {
            checkpoint_id,
            session_id,
            parent_commit,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
            created_at,
        } => Ok((
            format!("{base}; row can be rebuilt from the commit's metadata.json"),
            RepairPlan::InsertCatalogRow {
                checkpoint_id,
                session_id,
                parent_commit,
                tree_oid,
                metadata_blob_oid,
                traces_commit,
                created_at,
            },
        )),
    }
}

/// Walk `refs/libra/traces` first-parent and return checkpoint_id →
/// [`RefCheckpoint`] for every reachable checkpoint, attributed to its
/// introducing commit (newest introduction wins after prune rewrites).
/// Failures truncate with a note and under-detect — never over-report.
fn walk_traces_checkpoints(
    reader: &ObjectReader,
    head: Option<ObjectHash>,
    notes: &mut Vec<String>,
) -> (HashMap<String, RefCheckpoint>, bool) {
    let mut chain: Vec<Commit> = Vec::new();
    let mut cursor = head;
    while let Some(oid) = cursor {
        if chain.len() >= MAX_TRACES_WALK_COMMITS {
            notes.push(format!(
                "traces walk stopped after {MAX_TRACES_WALK_COMMITS} commits (cycle guard)"
            ));
            break;
        }
        match reader.read_commit(&oid) {
            Ok(commit) => {
                cursor = commit.parent_commit_ids.first().copied();
                chain.push(commit);
            }
            Err(_) => {
                notes.push("traces walk truncated because a commit could not be read".to_string());
                break;
            }
        }
    }

    let mut per_commit: Vec<HashMap<String, ObjectHash>> = Vec::with_capacity(chain.len());
    let mut has_noncanonical_checkpoint_id = false;
    for commit in &chain {
        match checkpoint_ids_in_commit(reader, commit) {
            Ok((ids, has_noncanonical_id)) => {
                has_noncanonical_checkpoint_id |= has_noncanonical_id;
                per_commit.push(ids);
            }
            Err(_) => {
                notes.push(
                    "a checkpoint tree is unreadable; reachability scan is incomplete".to_string(),
                );
                per_commit.push(HashMap::new());
            }
        }
    }

    let empty: HashMap<String, ObjectHash> = HashMap::new();
    let mut out: HashMap<String, RefCheckpoint> = HashMap::new();
    for (index, commit) in chain.iter().enumerate() {
        let parent_ids = per_commit.get(index + 1).unwrap_or(&empty);
        for (id, inner_oid) in &per_commit[index] {
            if parent_ids.contains_key(id) || out.contains_key(id) {
                continue;
            }
            let trailers = parse_libra_trailers(&commit.message);
            let mut rc = RefCheckpoint {
                commit: commit.id.to_string(),
                root_tree: commit.tree_id.to_string(),
                metadata_blob: None,
                manifest_present: false,
                metadata_present: false,
                inner_readable: false,
                parent_commit_trailer: trailers.parent_commit,
                scope_trailer: trailers.scope,
            };
            match reader.read_tree(inner_oid) {
                Ok(inner) => {
                    rc.inner_readable = true;
                    for item in &inner.tree_items {
                        match item.name.as_str() {
                            "manifest.json" => rc.manifest_present = true,
                            "metadata.json" => {
                                rc.metadata_present = true;
                                rc.metadata_blob = Some(item.id.to_string());
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => {
                    notes.push(
                        "a checkpoint inner tree is unreadable; reachability scan is incomplete"
                            .to_string(),
                    );
                }
            }
            out.insert(id.clone(), rc);
        }
    }
    (out, has_noncanonical_checkpoint_id)
}

/// Enumerate `checkpoint/<prefix>/<rest>` ids (and their inner tree OIDs)
/// in one commit's root tree.
fn checkpoint_ids_in_commit(
    reader: &ObjectReader,
    commit: &Commit,
) -> anyhow::Result<(HashMap<String, ObjectHash>, bool)> {
    let root = reader.read_tree(&commit.tree_id)?;
    let Some(checkpoint_entry) = root
        .tree_items
        .iter()
        .find(|item| item.name == "checkpoint" && item.mode == TreeItemMode::Tree)
    else {
        return Ok((HashMap::new(), false));
    };
    let checkpoint_tree = reader.read_tree(&checkpoint_entry.id)?;
    let mut out = HashMap::new();
    let mut has_noncanonical_checkpoint_id = false;
    for prefix in &checkpoint_tree.tree_items {
        if prefix.mode != TreeItemMode::Tree {
            continue;
        }
        let prefix_tree = reader.read_tree(&prefix.id)?;
        for rest in &prefix_tree.tree_items {
            if rest.mode != TreeItemMode::Tree {
                continue;
            }
            let checkpoint_id = format!("{}{}", prefix.name, rest.name);
            if let Some(checkpoint_id) = canonical_checkpoint_id(&checkpoint_id) {
                out.insert(checkpoint_id, rest.id);
            } else {
                has_noncanonical_checkpoint_id = true;
            }
        }
    }
    Ok((out, has_noncanonical_checkpoint_id))
}

/// Determine whether a catalog-only row (not ref-reachable) points at a
/// legacy-v1 checkpoint tree. Any read failure returns `false` — a row
/// whose layout cannot be proven legacy stays subject to class-1 checks.
fn row_layout_is_legacy_v1(reader: &ObjectReader, tree_oid: &str, checkpoint_id: &str) -> bool {
    let (Some(prefix), Some(rest)) = (checkpoint_id.get(..2), checkpoint_id.get(2..)) else {
        return false;
    };
    let Ok(root_oid) = crate::internal::object_format::parse_repo_oid(tree_oid) else {
        return false;
    };
    let Ok(root) = reader.read_tree(&root_oid) else {
        return false;
    };
    let mut current = root;
    for segment in ["checkpoint", prefix, rest] {
        let Some(entry) = current
            .tree_items
            .iter()
            .find(|item| item.name == segment && item.mode == TreeItemMode::Tree)
        else {
            return false;
        };
        let Ok(next) = reader.read_tree(&entry.id) else {
            return false;
        };
        current = next;
    }
    let manifest_present = current
        .tree_items
        .iter()
        .any(|item| item.name == "manifest.json");
    let metadata_present = current
        .tree_items
        .iter()
        .any(|item| item.name == "metadata.json");
    metadata_present && !manifest_present
}

/// Render the class-1 `missing_objects` detail from the collected
/// "path oid" descriptors (row columns first, then E4 sidecars).
fn missing_objects_detail(missing: &[String]) -> String {
    if missing.is_empty() {
        // Reached only via the stale-but-unverifiable branch.
        return "row disagrees with refs/libra/traces but the ref-side objects \
                could not be verified"
            .to_string();
    }
    format!(
        "objects missing from the store: {} — no destructive action taken",
        missing.join(", ")
    )
}

async fn load_catalog_rows(conn: &DatabaseConnection) -> CliResult<Vec<CatalogRow>> {
    let backend = conn.get_database_backend();
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, tree_oid, metadata_blob_oid, traces_commit \
             FROM agent_checkpoint ORDER BY created_at ASC, checkpoint_id ASC",
            [],
        ))
        .await
        .map_err(|_| {
            CliError::fatal(
                "agent doctor could not inspect checkpoint catalog rows; check the repository database and rerun doctor"
                    .to_string(),
            )
        })?;
    rows.into_iter()
        .map(|row| {
            Ok(CatalogRow {
                checkpoint_id: row.try_get_by("checkpoint_id").map_err(|_| {
                    CliError::fatal(
                        "agent doctor could not decode a checkpoint catalog row; check the repository database and rerun doctor"
                            .to_string(),
                    )
                })?,
                tree_oid: row.try_get_by("tree_oid").map_err(|_| {
                    CliError::fatal(
                        "agent doctor could not decode a checkpoint catalog row; check the repository database and rerun doctor"
                            .to_string(),
                    )
                })?,
                metadata_blob_oid: row.try_get_by("metadata_blob_oid").map_err(|_| {
                    CliError::fatal(
                        "agent doctor could not decode a checkpoint catalog row; check the repository database and rerun doctor"
                            .to_string(),
                    )
                })?,
                traces_commit: row.try_get_by("traces_commit").map_err(|_| {
                    CliError::fatal(
                        "agent doctor could not decode a checkpoint catalog row; check the repository database and rerun doctor"
                            .to_string(),
                    )
                })?,
            })
        })
        .collect()
}

async fn session_exists(conn: &DatabaseConnection, session_id: &str) -> CliResult<bool> {
    let backend = conn.get_database_backend();
    conn.query_one_raw(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM agent_session WHERE session_id = ? LIMIT 1",
        [session_id.into()],
    ))
    .await
    .map(|row| row.is_some())
    .map_err(|_| {
        CliError::fatal(
            "agent doctor could not inspect agent-session ownership; check the repository database and rerun doctor"
                .to_string(),
        )
    })
}

/// Same repo-id resolution as the background indexer
/// (`client_storage::resolve_repo_id_for_index`): the `libra.repoid`
/// config key, falling back to `unknown-repo`.
/// Blob OID (hex) of `bytes` WITHOUT writing — content addressing lets us
/// test whether an on-disk `findings.md` would restore the exact missing
/// object before touching the store.
fn blob_oid_hex(bytes: &[u8]) -> String {
    let header = format!("blob {}\0", bytes.len());
    let mut content = header.into_bytes();
    content.extend_from_slice(bytes);
    ObjectHash::new_for_kind(git_internal::hash::get_hash_kind(), &content).to_string()
}

/// Result of reading a review/investigate sidecar through a descriptor-pinned
/// run directory. Refusal variants deliberately carry neither a path nor
/// source bytes into doctor output.
enum AgentRunSidecar {
    Absent,
    Unavailable,
    Oversized,
    Bytes(Vec<u8>),
}

/// Read a regular sidecar from an already-open run directory. The directory
/// and leaf are held with no-follow descriptors, while the post-read stat
/// rejects replacement/growth races before a repair can hash or publish it.
fn read_agent_run_sidecar(run_dir: &std::fs::File, name: &str) -> AgentRunSidecar {
    let file = match crate::utils::object::open_regular_file_at_no_follow(
        run_dir,
        std::ffi::OsStr::new(name),
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AgentRunSidecar::Absent;
        }
        Err(_) => return AgentRunSidecar::Unavailable,
    };
    let Ok(initial_len) = file.metadata().map(|metadata| metadata.len()) else {
        return AgentRunSidecar::Unavailable;
    };
    if initial_len > DOCTOR_AGENT_RUN_SIDECAR_READ_CAP_BYTES {
        return AgentRunSidecar::Oversized;
    }
    let Ok(capacity) = usize::try_from(initial_len) else {
        return AgentRunSidecar::Oversized;
    };
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(capacity).is_err() {
        return AgentRunSidecar::Unavailable;
    }
    let mut reader = std::io::BufReader::new(file).take(initial_len);
    if reader.read_to_end(&mut bytes).is_err() || bytes.len() as u64 != initial_len {
        return AgentRunSidecar::Unavailable;
    }
    let Ok(final_len) = reader
        .into_inner()
        .into_inner()
        .metadata()
        .map(|metadata| metadata.len())
    else {
        return AgentRunSidecar::Unavailable;
    };
    if final_len != initial_len {
        return AgentRunSidecar::Unavailable;
    }
    AgentRunSidecar::Bytes(bytes)
}

fn append_findings_store_note(report: &mut FindingsStoreReport, note: &'static str) {
    match report.note.as_mut() {
        Some(existing) if !existing.contains(note) => {
            existing.push_str("; ");
            existing.push_str(note);
        }
        Some(_) => {}
        None => report.note = Some(note.to_string()),
    }
}

/// Minimal parse of a run manifest's non-null `findings_oid` (avoids pulling
/// the review/investigate manifest structs into doctor). A damaged manifest
/// must never reflect its arbitrary string back into a diagnostic.
enum ManifestFindingsOid {
    Absent,
    Valid(String),
    Invalid,
    Unavailable,
}

fn manifest_findings_oid(run_dir: &std::fs::File) -> ManifestFindingsOid {
    let bytes = match read_agent_run_sidecar(run_dir, "manifest.json") {
        AgentRunSidecar::Bytes(bytes) => bytes,
        AgentRunSidecar::Absent => return ManifestFindingsOid::Absent,
        AgentRunSidecar::Unavailable | AgentRunSidecar::Oversized => {
            return ManifestFindingsOid::Unavailable;
        }
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ManifestFindingsOid::Absent;
    };
    let Some(value) = value.get("findings_oid").and_then(|value| value.as_str()) else {
        return ManifestFindingsOid::Absent;
    };
    crate::internal::object_format::parse_repo_oid(value)
        .ok()
        .map(|oid| ManifestFindingsOid::Valid(oid.to_string()))
        .unwrap_or(ManifestFindingsOid::Invalid)
}

/// A0-06: scan review/investigate run manifests for findings-object
/// inconsistencies, mirroring the checkpoint-store scan scoped to the
/// `findings_oid` blob:
/// - `missing_findings_object`: the blob is absent — AUTO-repairable when an
///   on-disk `findings.md` re-hashes to the same OID (content-addressed, so
///   the rewrite is exact + idempotent); MANUAL when `findings.md` is gone or
///   changed.
/// - `missing_findings_object_index`: the blob is present but has no/drifted
///   `object_index` row — re-inserted so cloud sync / retention see it.
///
/// Doctor is foreground: repairs write the blob + insert the `object_index`
/// row directly (idempotent), never the background enqueue.
async fn scan_agent_findings(
    conn: &DatabaseConnection,
    schema_present: bool,
    repair: bool,
) -> CliResult<FindingsStoreReport> {
    use crate::internal::ai::review::store::{AGENT_FINDINGS_OTYPE, is_valid_run_id};

    let mut report = FindingsStoreReport {
        scanned: false,
        note: None,
        runs_with_findings: 0,
        repair_applied: repair,
        repaired: 0,
        manual_required: 0,
        findings: Vec::new(),
    };
    if !schema_present {
        report.note = Some("agent schema not present (run `libra init`?)".to_string());
        return Ok(report);
    }
    let repo_path = match util::try_get_storage_path(None) {
        Ok(path) => path,
        Err(_) => {
            report.note = Some("repository storage unavailable".to_string());
            return Ok(report);
        }
    };
    report.scanned = true;
    if !ObjectReader::secure_reads_supported() {
        report.repair_applied = false;
        report.note = Some(
            "agent-run findings scan is unavailable because this platform lacks secure descriptor-relative reads; no repairs were applied"
                .to_string(),
        );
        return Ok(report);
    }
    // See the checkpoint scan: doctor must not resolve externally controlled
    // alternate paths while producing a bounded diagnostic.
    let reader = ObjectReader {
        repo_path: repo_path.clone(),
    };
    let repo_id = resolve_repo_id(conn).await;
    let runs_root = repo_path.join("sessions").join("agent-runs");
    let runs_root_dir = match crate::utils::object::open_directory_tree_no_follow(&runs_root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(_) => {
            report.note = Some("agent-run storage is unavailable for a secure scan".to_string());
            return Ok(report);
        }
    };
    // Enumerate the held descriptor, rather than `runs_root` by pathname.
    // Otherwise an attacker could replace `sessions/agent-runs` after the
    // no-follow open above and make `read_dir` follow their replacement.
    let entries = match runs_root_dir
        .try_clone()
        .and_then(crate::utils::beneath::read_dir_fd)
    {
        Ok(entries) => entries,
        Err(_) => {
            report.note = Some("agent-run storage is unavailable for a secure scan".to_string());
            return Ok(report);
        }
    };
    for (entry_index, entry) in entries.enumerate() {
        if entry_index >= DOCTOR_AGENT_RUN_ENTRY_CAP {
            append_findings_store_note(
                &mut report,
                "agent-run storage exceeds the doctor entry limit; remaining runs require manual review",
            );
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                append_findings_store_note(
                    &mut report,
                    "agent-run storage could not be enumerated safely; remaining runs require manual review",
                );
                break;
            }
        };
        let run_name = entry.name;
        let Some(run_id) = run_name.to_str().map(str::to_string) else {
            continue;
        };
        // Skip `.admission` and any foreign directory — same validity gate the
        // run stores use.
        if !is_valid_run_id(&run_id) {
            continue;
        }
        let run_dir = match crate::utils::object::open_directory_at_no_follow(
            &runs_root_dir,
            &run_name,
        ) {
            Ok(directory) => directory,
            Err(_) => {
                append_findings_store_note(
                    &mut report,
                    "some agent-run entries were skipped because they could not be opened safely",
                );
                continue;
            }
        };
        let findings_oid = manifest_findings_oid(&run_dir);
        if matches!(findings_oid, ManifestFindingsOid::Absent) {
            continue;
        }
        report.runs_with_findings += 1;
        let findings_oid = match findings_oid {
            ManifestFindingsOid::Valid(oid) => oid,
            ManifestFindingsOid::Invalid => {
                report.manual_required += 1;
                report.findings.push(FindingsObjectFinding {
                    inconsistency_type: CLASS_MISSING_FINDINGS_OBJECT.to_string(),
                    run_id,
                    detail:
                        "findings manifest declares an invalid object identifier; manual review"
                            .to_string(),
                    repaired: false,
                    manual_required: true,
                });
                continue;
            }
            ManifestFindingsOid::Unavailable => {
                append_findings_store_note(
                    &mut report,
                    "some agent-run manifests were skipped because they could not be read safely",
                );
                continue;
            }
            ManifestFindingsOid::Absent => continue,
        };

        let on_disk = match read_agent_run_sidecar(&run_dir, "findings.md") {
            AgentRunSidecar::Bytes(bytes) if !bytes.is_empty() => Some(bytes),
            AgentRunSidecar::Bytes(_) | AgentRunSidecar::Absent => None,
            AgentRunSidecar::Unavailable => {
                append_findings_store_note(
                    &mut report,
                    "some findings sidecars were skipped because they could not be read safely",
                );
                None
            }
            AgentRunSidecar::Oversized => {
                append_findings_store_note(
                    &mut report,
                    "some findings sidecars exceed the doctor read limit and require manual review",
                );
                None
            }
        };

        if !reader.exists_str(&findings_oid) {
            // Auto-repairable only when findings.md is present AND re-hashes to
            // the missing OID (content-addressed → the rewrite is exact and
            // idempotent). `filter` binds the bytes without any fallible unwrap.
            let recoverable = on_disk
                .as_ref()
                .filter(|bytes| blob_oid_hex(bytes) == findings_oid);
            if let Some(bytes) = recoverable {
                let (repaired, detail) = if repair {
                    // §C.4.3 writer-vs-deleter: rewriting the blob and
                    // re-inserting its `object_index` row is a PUBLICATION,
                    // and `agent` is excluded from the command-level shared
                    // hold (its runs are long). Without this the repair could
                    // land inside a deletion phase and be pruned right back
                    // out — repairing the same run forever.
                    let _publication = crate::internal::maintenance_lock::MaintenanceLock::shared(
                        &repo_path,
                    )
                    .map_err(|_| {
                        CliError::fatal(
                            "agent doctor could not acquire the findings repair lock; resolve the store condition and rerun doctor"
                                .to_string(),
                        )
                    })?;
                    crate::utils::object::write_git_object(&repo_path, "blob", bytes).map_err(
                        |_| {
                            CliError::fatal(
                                "agent doctor could not rewrite a findings object; resolve the store condition and rerun doctor"
                                    .to_string(),
                            )
                        },
                    )?;
                    insert_object_index_row(
                        conn,
                        &findings_oid,
                        AGENT_FINDINGS_OTYPE,
                        bytes.len() as i64,
                        &repo_id,
                    )
                    .await
                    .map_err(|_| {
                        CliError::fatal(
                            "agent doctor could not reindex a findings object; resolve the store condition and rerun doctor"
                                .to_string(),
                        )
                    })?;
                    report.repaired += 1;
                    (
                        true,
                        format!(
                            "findings object {findings_oid} was missing; rewrote it from findings.md"
                        ),
                    )
                } else {
                    (
                        false,
                        format!(
                            "findings object {findings_oid} is missing but findings.md matches — repairable"
                        ),
                    )
                };
                report.findings.push(FindingsObjectFinding {
                    inconsistency_type: CLASS_MISSING_FINDINGS_OBJECT.to_string(),
                    run_id,
                    detail,
                    repaired,
                    manual_required: false,
                });
            } else {
                report.manual_required += 1;
                report.findings.push(FindingsObjectFinding {
                    inconsistency_type: CLASS_MISSING_FINDINGS_OBJECT.to_string(),
                    run_id,
                    detail: format!(
                        "findings object {findings_oid} is missing and findings.md is absent or changed — manual review"
                    ),
                    repaired: false,
                    manual_required: true,
                });
            }
            continue;
        }

        // Blob present — ensure a correctly-shaped object_index row.
        let Ok(hash) = crate::internal::object_format::parse_repo_oid(&findings_oid) else {
            continue;
        };
        let o_size = match reader.read_raw(&hash).and_then(|bytes| {
            i64::try_from(bytes.len())
                .map_err(|_| anyhow::anyhow!("findings object size exceeds index range"))
        }) {
            Ok(size) => size,
            Err(_) => {
                // The existence probe and payload read are separate filesystem
                // operations. If the object disappeared or became unreadable
                // in between, do not turn a healthy index row into size zero
                // (and do not manufacture a new row). A later safe scan can
                // retry from fresh evidence.
                let note =
                    "findings object index check skipped because a local object could not be read";
                match report.note.as_mut() {
                    Some(existing) => {
                        existing.push_str("; ");
                        existing.push_str(note);
                    }
                    None => report.note = Some(note.to_string()),
                }
                continue;
            }
        };
        let shape = object_index_row_shape(conn, &findings_oid, &repo_id).await?;
        // The o_type is cosmetic here: doctor enumerates findings from the
        // MANIFEST (`findings_oid`), not from `object_index`, and cloud sync
        // does not filter by o_type. The shared index consumer also refuses to
        // retag an already-`agent_*` row, so a findings blob whose bytes were
        // first indexed as `agent_transcript` legitimately keeps that tag.
        // Flag only a genuinely-untracked (absent) or wrong-SIZE row; never a
        // benign tag difference — that would start a doctor↔writer tag-war.
        let needs_repair = match &shape {
            None => true,
            Some((_o_type, size)) => *size != o_size,
        };
        if needs_repair {
            let repaired = if repair {
                if shape.is_some() {
                    update_object_index_row_shape(
                        conn,
                        &findings_oid,
                        AGENT_FINDINGS_OTYPE,
                        o_size,
                        &repo_id,
                    )
                    .await
                    .map_err(|_| {
                        CliError::fatal(
                            "agent doctor could not update a findings object index; resolve the store condition and rerun doctor"
                                .to_string(),
                        )
                    })?;
                } else {
                    insert_object_index_row(
                        conn,
                        &findings_oid,
                        AGENT_FINDINGS_OTYPE,
                        o_size,
                        &repo_id,
                    )
                    .await
                    .map_err(|_| {
                        CliError::fatal(
                            "agent doctor could not insert a findings object index; resolve the store condition and rerun doctor"
                                .to_string(),
                        )
                    })?;
                }
                report.repaired += 1;
                true
            } else {
                false
            };
            report.findings.push(FindingsObjectFinding {
                inconsistency_type: CLASS_MISSING_FINDINGS_OBJECT_INDEX.to_string(),
                run_id,
                detail: format!("findings object {findings_oid} has no/drifted object_index row"),
                repaired,
                manual_required: false,
            });
        }
    }
    Ok(report)
}

async fn resolve_repo_id<C: ConnectionTrait>(conn: &C) -> String {
    match ConfigKv::get_with_conn(conn, "libra.repoid").await {
        Ok(Some(entry)) if !entry.value.trim().is_empty() => entry.value,
        _ => "unknown-repo".to_string(),
    }
}

/// Existing `object_index` row shape for `(o_id, repo_id)`, if any.
/// Class-3 compares it against the writer's expected `o_type`/`o_size` —
/// a row that exists but drifted (e.g. a transcript blob indexed as a
/// generic `blob`) is as broken for cloud-sync semantics as a missing
/// one and must be repairable in place.
async fn object_index_row_shape<C: ConnectionTrait>(
    conn: &C,
    o_id: &str,
    repo_id: &str,
) -> CliResult<Option<(String, i64)>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT o_type, o_size FROM object_index WHERE o_id = ? AND repo_id = ? LIMIT 1",
            [o_id.into(), repo_id.into()],
        ))
        .await
        .map_err(|_| {
            CliError::fatal(
                "agent doctor could not inspect object-index state; check the repository database and rerun doctor"
                    .to_string(),
            )
        })?;
    row.map(|r| {
        let o_type: String = r
            .try_get("", "o_type")
            .map_err(|_| {
                CliError::fatal(
                    "agent doctor could not decode object-index state; check the repository database and rerun doctor"
                        .to_string(),
                )
            })?;
        let o_size: i64 = r
            .try_get("", "o_size")
            .map_err(|_| {
                CliError::fatal(
                    "agent doctor could not decode object-index state; check the repository database and rerun doctor"
                        .to_string(),
                )
            })?;
        Ok::<_, CliError>((o_type, o_size))
    })
    .transpose()
}

/// Update a drifted `object_index` row in place to the writer-expected
/// shape (idempotent; matched on `(o_id, repo_id)`).
async fn update_object_index_row_shape<C: ConnectionTrait>(
    conn: &C,
    o_id: &str,
    o_type: &str,
    o_size: i64,
    repo_id: &str,
) -> Result<(), sea_orm::DbErr> {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "UPDATE object_index
         SET o_type = ?, o_size = ?, is_synced = 0
         WHERE o_id = ? AND repo_id = ?",
        [o_type.into(), o_size.into(), o_id.into(), repo_id.into()],
    ))
    .await
    .map(|_| ())
}

/// Idempotent `object_index` insert mirroring the row shape written by
/// `client_storage::update_object_index_once` (`is_synced = 0` so the next
/// `libra cloud sync` picks the object up). The `WHERE NOT EXISTS` guard
/// makes a doctor re-run (or a race with the background indexer) a no-op.
async fn insert_object_index_row<C: ConnectionTrait>(
    conn: &C,
    o_id: &str,
    o_type: &str,
    o_size: i64,
    repo_id: &str,
) -> Result<(), sea_orm::DbErr> {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) \
         SELECT ?, ?, ?, ?, ?, 0 \
         WHERE NOT EXISTS (SELECT 1 FROM object_index WHERE o_id = ? AND repo_id = ?)",
        [
            o_id.into(),
            o_type.into(),
            o_size.into(),
            repo_id.into(),
            Utc::now().timestamp().into(),
            o_id.into(),
            repo_id.into(),
        ],
    ))
    .await
    .map(|_| ())
}

/// Apply one class-3 repair plan as a single database mutation. The plan is
/// built only after all object validation has finished, but a later DML can
/// still fail (for example, because a local database trigger or constraint
/// rejects a row). Rolling that failure back prevents a partially repaired
/// checkpoint from becoming cloud-visible while its sibling objects remain
/// missing from `object_index`.
async fn repair_object_index_rows_atomically(
    conn: &DatabaseConnection,
    entries: &[(String, String, i64)],
    updates: &[(String, String, i64)],
    repo_id: &str,
) -> Result<()> {
    const OPERATION: &str = "doctor object-index repair";
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .with_context(|| format!("begin {OPERATION}"))?;

    for (oid, o_type, o_size) in entries {
        if let Err(error) = insert_object_index_row(&txn, oid, o_type, *o_size, repo_id).await {
            txn.rollback().await.ok();
            return Err(error).with_context(|| format!("insert checkpoint object index row {oid}"));
        }
    }
    for (oid, o_type, o_size) in updates {
        if let Err(error) = update_object_index_row_shape(&txn, oid, o_type, *o_size, repo_id).await
        {
            txn.rollback().await.ok();
            return Err(error).with_context(|| format!("update checkpoint object index row {oid}"));
        }
    }

    txn.commit()
        .await
        .with_context(|| format!("commit {OPERATION}"))
}

/// Foreground, idempotent repair used by historical-import replay after its
/// previous background object-index barrier timed out or observed an update
/// error. The caller runs this function in a killable helper process, so local
/// or tiered object reads cannot extend the import command's absolute deadline.
/// Transcript payloads are never materialized: row sizes come from a
/// descriptor-pinned streaming integrity check under both per-object and
/// aggregate validation budgets.
pub(crate) struct SessionObjectIndexRepairRequest<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) marker_owner: &'a str,
    pub(crate) marker_generation: &'a str,
    pub(crate) agent_kind: &'a str,
    pub(crate) provider_session_id: &'a str,
    pub(crate) capture_scope: &'a CaptureScope,
}

pub(crate) async fn repair_session_object_index(
    conn: &DatabaseConnection,
    repo_path: &Path,
    request: SessionObjectIndexRepairRequest<'_>,
) -> anyhow::Result<usize> {
    let SessionObjectIndexRepairRequest {
        session_id,
        marker_owner,
        marker_generation,
        agent_kind,
        provider_session_id,
        capture_scope,
    } = request;
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .context("begin fenced import object-index repair")?;
    if let Err(error) = capture_scope.assert_workspace_fence_live(&txn).await {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before import object-index repair");
    }
    // Acquire the SQLite writer slot before reading either the marker or the
    // session. Erasure uses the same database writer serialization, so it
    // cannot prune the catalog and then race these index inserts back in.
    let locked = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE metadata_kv SET updated_at = updated_at
             WHERE scope = 'agent_import_index_repair'
               AND target = ? AND key = 'object-index-v1'",
            [session_id.into()],
        ))
        .await
        .context("lock import object-index repair marker")?;
    if locked.rows_affected() != 1 {
        bail!("import object-index repair marker disappeared or is no longer owned");
    }
    let marker = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT value FROM metadata_kv
             WHERE scope = 'agent_import_index_repair'
               AND target = ? AND key = 'object-index-v1'",
            [session_id.into()],
        ))
        .await
        .context("read fenced import object-index repair marker")?
        .context("import object-index repair marker disappeared after lock")?;
    let marker_value: String = marker
        .try_get_by("value")
        .context("decode fenced import object-index repair marker")?;
    let marker_json: serde_json::Value = serde_json::from_str(&marker_value)
        .context("decode fenced import object-index repair marker JSON")?;
    let marker_scope = marker_json
        .get("capture_scope")
        .cloned()
        .context("import object-index repair marker has no capture scope")
        .and_then(|value| {
            serde_json::from_value::<CaptureScope>(value)
                .context("decode import object-index repair marker capture scope")
        })?;
    if marker_json.get("owner").and_then(serde_json::Value::as_str) != Some(marker_owner)
        || marker_json
            .get("generation")
            .and_then(serde_json::Value::as_str)
            != Some(marker_generation)
        || marker_json
            .get("agent_kind")
            .and_then(serde_json::Value::as_str)
            != Some(agent_kind)
        || marker_json
            .get("provider_session_id")
            .and_then(serde_json::Value::as_str)
            != Some(provider_session_id)
        || marker_scope != *capture_scope
    {
        bail!("import object-index repair marker ownership changed");
    }
    let tombstone = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ?",
            [agent_kind.into(), provider_session_id.into()],
        ))
        .await
        .context("check erasure tombstone during import object-index repair")?;
    if tombstone.is_some() {
        bail!("session was erased while its import object-index repair was pending");
    }
    let session = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT agent_kind, provider_session_id FROM agent_session WHERE session_id = ?",
            [session_id.into()],
        ))
        .await
        .context("validate session ownership during import object-index repair")?;
    if let Some(session) = session {
        let stored_agent_kind: String = session
            .try_get_by("agent_kind")
            .context("decode repair session agent kind")?;
        let stored_provider_session_id: String = session
            .try_get_by("provider_session_id")
            .context("decode repair provider session id")?;
        if stored_agent_kind != agent_kind || stored_provider_session_id != provider_session_id {
            bail!("session ownership changed while import object-index repair was pending");
        }
    }
    import_index_repair_test_pause_after_lock()?;
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT checkpoint_id, tree_oid, traces_commit
             FROM agent_checkpoint
             WHERE session_id = ?
             ORDER BY created_at, checkpoint_id
             LIMIT ?",
            [
                session_id.into(),
                i64::try_from(MAX_IMPORT_INDEX_REPAIR_CHECKPOINTS + 1)
                    .context("import index repair checkpoint bound overflow")?
                    .into(),
            ],
        ))
        .await
        .context("load checkpoints for import object-index repair")?;
    if rows.len() > MAX_IMPORT_INDEX_REPAIR_CHECKPOINTS {
        bail!(
            "session exceeds the bounded import object-index repair limit; run `libra agent doctor --repair`"
        );
    }

    let reader = ObjectReader {
        repo_path: repo_path.to_path_buf(),
    };
    let repo_id = resolve_repo_id(&txn).await;
    // Validate every target before issuing any object-index DML. A later
    // corrupt blob or exhausted aggregate budget must not leave an earlier
    // checkpoint partially indexed if this fenced import repair aborts.
    let mut validation_budget = ObjectIndexValidationBudget::new();
    let mut seen = BTreeSet::new();
    let validated_targets = (|| -> anyhow::Result<Vec<(String, &'static str, i64)>> {
        let mut targets_to_write = Vec::new();
        for row in rows {
            let checkpoint_id: String = row
                .try_get_by("checkpoint_id")
                .context("decode import index repair checkpoint id")?;
            let tree_oid: String = row
                .try_get_by("tree_oid")
                .context("decode import index repair root tree")?;
            let traces_commit: String = row
                .try_get_by("traces_commit")
                .context("decode import index repair traces commit")?;
            let sweep = sweep_e4_checkpoint_objects(&reader, &tree_oid, &checkpoint_id);
            if !sweep.manifest_present {
                continue;
            }
            if !sweep.missing.is_empty() {
                bail!(
                    "checkpoint {checkpoint_id} is missing object-store data required for index repair: {}; run `libra agent doctor --repair`",
                    sweep.missing.join(", ")
                );
            }
            let mut targets = vec![(traces_commit, "commit")];
            targets.extend(
                sweep
                    .present
                    .into_iter()
                    .map(|object| (object.oid, object.o_type)),
            );
            for (oid, o_type) in targets {
                if !seen.insert(oid.clone()) {
                    continue;
                }
                let hash = crate::internal::object_format::parse_repo_oid(&oid)
                    .map_err(|_| anyhow::anyhow!("invalid checkpoint object identifier"))?;
                let size = validation_budget
                    .validate(&reader, &hash, expected_git_object_type(o_type))
                    .context("integrity-check checkpoint object for index repair")?;
                targets_to_write.push((oid, o_type, size));
            }
        }
        Ok(targets_to_write)
    })();
    let validated_targets = match validated_targets {
        Ok(targets) => targets,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };

    let mut repaired = 0_usize;
    for (oid, o_type, size) in validated_targets {
        match object_index_row_shape(&txn, &oid, &repo_id)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
        {
            Some((existing_type, existing_size))
                if existing_type == o_type && existing_size == size => {}
            Some(_) => {
                update_object_index_row_shape(&txn, &oid, o_type, size, &repo_id)
                    .await
                    .with_context(|| format!("repair object-index row {oid}"))?;
                repaired = repaired.saturating_add(1);
            }
            None => {
                insert_object_index_row(&txn, &oid, o_type, size, &repo_id)
                    .await
                    .with_context(|| format!("insert object-index row {oid}"))?;
                repaired = repaired.saturating_add(1);
            }
        }
    }
    commit_session_object_index_repair(txn, capture_scope).await?;
    Ok(repaired)
}

/// Finish an import object-index repair only while the originating workspace
/// lease is live. The conditional workspace update is deliberately the final
/// DML so an expiry after a repaired row was written rolls that row back.
async fn commit_session_object_index_repair(
    txn: DatabaseTransaction,
    capture_scope: &CaptureScope,
) -> anyhow::Result<()> {
    if let Err(error) = capture_scope
        .assert_workspace_fence_live_for_commit(&txn)
        .await
    {
        txn.rollback().await.ok();
        return Err(error).context(
            "verify capture workspace lease before committing import object-index repair",
        );
    }
    txn.commit()
        .await
        .context("commit fenced import object-index repair")
}

// ---------------------------------------------------------------------------
// Provider hooks + report rendering
// ---------------------------------------------------------------------------

fn check_provider(
    name: &'static str,
    tier: AgentStability,
    provider: Option<&dyn crate::internal::ai::hooks::provider::HookProvider>,
) -> ProviderHookStatus {
    let Some(provider) = provider else {
        // Preview adapters don't carry a HookProvider yet. Surface them
        // explicitly as preview/unknown so the report is still complete.
        return ProviderHookStatus {
            name,
            tier,
            installed: None,
            error: None,
        };
    };
    match provider.hooks_are_installed() {
        Ok(installed) => ProviderHookStatus {
            name,
            tier,
            installed: Some(installed),
            error: None,
        },
        Err(_) => ProviderHookStatus {
            name,
            tier,
            installed: None,
            error: Some(
                "provider hook settings could not be inspected; verify the provider configuration and rerun doctor"
                    .to_string(),
            ),
        },
    }
}

fn emit_report(report: &DoctorReport, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("agent_doctor", report, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!(
        "Schema present       : {}",
        if report.schema_present { "yes" } else { "no" }
    );
    println!("Active sessions      : {}", report.active_sessions);
    println!("Stopped sessions     : {}", report.stopped_sessions);
    println!("Orphan checkpoints   : {}", report.orphan_checkpoints);
    if report.legacy_code_residue.paths.is_empty()
        && report.legacy_code_residue.intent_ref.is_none()
    {
        println!("Frozen Code residue  : none");
    } else {
        let mut items = report.legacy_code_residue.paths.clone();
        if let Some(intent_ref) = &report.legacy_code_residue.intent_ref {
            items.push(intent_ref.clone());
        }
        println!("Frozen Code residue  : {}", items.join(", "));
        println!("  note: {}", report.legacy_code_residue.note);
    }

    println!("Provider hooks:");
    for ph in &report.provider_hooks {
        let tier_tag = match ph.tier {
            AgentStability::Preview => " [preview]",
            AgentStability::Stable => "",
        };
        match (ph.installed, &ph.error) {
            (Some(true), _) => println!("  {}{tier_tag}: installed", ph.name),
            (Some(false), _) => println!("  {}{tier_tag}: NOT installed", ph.name),
            (None, Some(err)) => println!("  {}{tier_tag}: error — {err}", ph.name),
            (None, None) => println!("  {}{tier_tag}: not yet installable", ph.name),
        }
    }

    let store = &report.checkpoint_store;
    println!("Checkpoint store:");
    if !store.scanned {
        println!(
            "  not scanned ({})",
            store.note.as_deref().unwrap_or("unavailable")
        );
    } else {
        println!("  Catalog rows          : {}", store.catalog_rows);
        println!(
            "  Ref-reachable         : {}",
            store.ref_reachable_checkpoints
        );
        println!("  Legacy-v1 checkpoints : {}", store.legacy_v1_checkpoints);
        println!("  Live in-flight markers: {}", store.live_inflight_markers);
        println!("  Inconsistencies       : {}", store.findings.len());
        for finding in &store.findings {
            let status = if finding.repaired {
                "repaired"
            } else if finding.manual_required {
                "manual action required"
            } else if store.repair_applied {
                "NOT repaired"
            } else {
                "detected (run --repair)"
            };
            println!(
                "    [{}] {}: {} — {status}",
                finding.inconsistency_type, finding.checkpoint_id, finding.detail
            );
        }
        if let Some(note) = &store.note {
            println!("  Note: {note}");
        }
    }

    let findings = &report.findings_store;
    println!("Findings store:");
    if !findings.scanned {
        println!(
            "  not scanned ({})",
            findings.note.as_deref().unwrap_or("unavailable")
        );
    } else {
        println!("  Runs with findings    : {}", findings.runs_with_findings);
        println!("  Inconsistencies       : {}", findings.findings.len());
        for finding in &findings.findings {
            let status = if finding.repaired {
                "repaired"
            } else if finding.manual_required {
                "manual action required"
            } else if findings.repair_applied {
                "NOT repaired"
            } else {
                "detected (run --repair)"
            };
            println!(
                "    [{}] {}: {} — {status}",
                finding.inconsistency_type, finding.run_id, finding.detail
            );
        }
        if let Some(note) = &findings.note {
            println!("  Note: {note}");
        }
    }

    if report.orphan_checkpoints > 0 {
        println!(
            "Hint: orphan checkpoints indicate broken FK cascade — \
             consider `libra agent clean --all`."
        );
    }
    let auto_repairable = store
        .findings
        .iter()
        .filter(|f| !f.repaired && !f.manual_required)
        .count();
    if !store.repair_applied && auto_repairable > 0 {
        println!(
            "Hint: run `libra agent doctor --repair` to repair {auto_repairable} \
             inconsistency(ies) automatically."
        );
    }
    let unresolved_links = store
        .findings
        .iter()
        .filter(|finding| {
            finding.manual_required && finding.inconsistency_type == CLASS_UNRESOLVED_SUBAGENT_LINK
        })
        .count();
    let missing_objects = store
        .findings
        .iter()
        .filter(|finding| {
            finding.manual_required && finding.inconsistency_type == CLASS_MISSING_OBJECTS
        })
        .count();
    let other_manual = store
        .manual_required
        .saturating_sub(unresolved_links.saturating_add(missing_objects));
    if unresolved_links > 0 {
        println!(
            "Hint: {unresolved_links} subagent content link(s) have no unique provider-stable \
             boundary; the content remains durable and no object repair is needed. Keep the \
             association unresolved unless provider-stable boundary evidence becomes available."
        );
    }
    if missing_objects > 0 {
        println!(
            "Hint: {} inconsistency(ies) need manual action — objects are missing \
             from the store (try `libra fsck --heal` or restore them from a \
             cloud/backup remote before re-running doctor).",
            missing_objects
        );
    }
    if other_manual > 0 {
        println!(
            "Hint: {other_manual} other inconsistency(ies) need manual action; inspect the \
             finding details and choose an explicit recovery instead of guessing."
        );
    }
    if report.gemini_hooks_remnant {
        println!(
            "Hint: leftover gemini hook configuration detected — gemini capture is \
             uninstall-only; run `libra agent remove gemini` to remove it. Captured \
             gemini sessions stay readable."
        );
    }
    if !report.schema_present {
        println!("Hint: run `libra init` to apply pending migrations.");
    }
    Ok(())
}

async fn table_exists(conn: &(impl ConnectionTrait + ?Sized), name: &str) -> CliResult<bool> {
    let backend = conn.get_database_backend();
    conn.query_one_raw(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
        [name.into()],
    ))
    .await
    .map(|row| row.is_some())
    .map_err(|_| {
        CliError::fatal(
            "agent doctor could not inspect repository schema; check the repository database and rerun doctor"
                .to_string(),
        )
    })
}

async fn scalar_count(conn: &(impl ConnectionTrait + ?Sized), sql: &str) -> CliResult<i64> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(backend, sql, []))
        .await
        .map_err(|_| {
            CliError::fatal(
                "agent doctor could not query repository state; check the repository database and rerun doctor"
                    .to_string(),
            )
        })?
        .ok_or_else(|| CliError::fatal("doctor count returned no rows".to_string()))?;
    row.try_get_by::<i64, _>("n").map_err(|_| {
        CliError::fatal(
            "agent doctor could not decode repository state; check the repository database and rerun doctor"
                .to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use sea_orm::{Database, DbBackend};

    use super::*;

    /// The handler's own storage/database mappings keep the stable codes of
    /// the generic repository preflight that doctor skips, and never embed
    /// the path carried by the underlying `io::Error`.
    #[test]
    fn storage_and_database_failures_keep_preflight_stable_codes_without_paths() {
        const PATH_CANARY: &str = "/doctor-path-canary/.libra";
        let not_found = doctor_storage_resolution_error(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{PATH_CANARY} is not a libra repository"),
        ));
        assert_eq!(not_found.stable_code(), StableErrorCode::RepoNotFound);
        assert_eq!(not_found.stable_code().exit_code().as_i32(), 128);

        let detached = doctor_storage_resolution_error(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("this worktree was removed from the registry (detached): {PATH_CANARY}"),
        ));
        assert_eq!(detached.stable_code(), StableErrorCode::RepoStateInvalid);
        assert_eq!(detached.stable_code().exit_code().as_i32(), 128);

        let missing_db = doctor_database_open_error(std::io::ErrorKind::NotFound);
        assert_eq!(missing_db.stable_code(), StableErrorCode::RepoCorrupt);
        assert_eq!(missing_db.stable_code().exit_code().as_i32(), 128);
        assert!(
            missing_db
                .message()
                .contains("repository database not found")
        );

        let unopenable = doctor_database_open_error(std::io::ErrorKind::Other);
        assert_eq!(unopenable.stable_code(), StableErrorCode::IoReadFailed);
        assert_eq!(unopenable.stable_code().exit_code().as_i32(), 128);

        for error in [not_found, detached, missing_db, unopenable] {
            assert!(
                !error.render_json().contains(PATH_CANARY),
                "doctor storage errors must stay path-free: {}",
                error.render_json()
            );
        }
    }

    #[test]
    fn artifact_replay_batch_shares_deadline_and_caps_attempts() {
        let mut batch = ArtifactReplayBatch::new();
        let deadline = batch.deadline().expect("establish batch deadline");
        assert_eq!(batch.deadline().expect("reuse batch deadline"), deadline);

        for _ in 0..ArtifactReplayBatch::MAX_ATTEMPTS {
            assert!(batch.has_capacity());
            batch.record_attempt();
        }

        assert!(!batch.has_capacity());
        assert_eq!(batch.deadline().expect("deadline remains pinned"), deadline);
    }

    #[test]
    fn manual_artifact_attempt_gets_a_fresh_bounded_deadline() {
        let mut batch = ArtifactReplayBatch::new();
        let automatic = batch.attempt_deadline(false).expect("automatic deadline");
        std::thread::sleep(Duration::from_millis(5));
        let manual = batch.attempt_deadline(true).expect("fresh manual deadline");
        assert!(manual.monotonic() > automatic.monotonic());
        assert_eq!(batch.attempt_deadline(false).unwrap(), automatic);
    }

    #[tokio::test]
    async fn stale_pending_artifact_is_quarantined_without_entering_worker_queue() {
        let dir = tempfile::tempdir().expect("create doctor test directory");
        let conn = crate::internal::db::create_database(
            dir.path()
                .join("doctor-stale-artifact.db")
                .to_str()
                .expect("database path is UTF-8"),
        )
        .await
        .expect("create doctor test database");
        crate::internal::config::ConfigKv::set_with_conn(
            &conn,
            "libra.repoid",
            "opaque-repository",
            false,
        )
        .await
        .expect("set doctor test repository identity");
        let scope = CaptureScope {
            repo_id: "opaque-repository".into(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        let header = format!(
            "{{\"version\":1,\"binding\":{{\"scope\":{{\"repo_id\":\"{}\",\"worktree_id\":\"\",\"workspace_id\":null,\"workspace_fence\":null}},\"session_id\":\"{}\",\"checkpoint_id\":\"{}\",\"event_id\":\"{}\",\"action_key\":\"action/test\",\"receipt_key\":\"receipt/test\",\"marker_generation\":\"marker/test\",\"source_commitment\":\"source/hmac-v2/{}\",\"reserved_revision\":1,\"original_deadline_millis\":null,\"deferrable\":true,\"first_attempt_millis\":1,\"parent_commit\":null,\"parent_unborn\":true}},\"mac\":\"pending-envelope/hmac-v1/{}\",\"envelope_bytes\":1,\"chunks\":1,\"manual_attempted\":false}}",
            scope.repo_id,
            uuid::Uuid::new_v4(),
            checkpoint_id,
            uuid::Uuid::new_v4(),
            "a".repeat(64),
            "c".repeat(64),
        );
        crate::internal::metadata::MetadataKv::set_with_conn(
            &conn,
            crate::internal::metadata::MetadataScope::AgentCapturePending,
            "opaque-repository",
            &checkpoint_id,
            &header,
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("insert exact pending artifact header");

        quarantine_stale_pending_artifact(&conn, &scope, &checkpoint_id)
            .await
            .expect("quarantine stale pending artifact");
        assert!(
            !pending::has_pending_header_for_checkpoint(&conn, "opaque-repository", &checkpoint_id)
                .await
                .expect("probe pending header")
        );
        assert!(
            pending::has_header_for_checkpoint(&conn, "opaque-repository", &checkpoint_id)
                .await
                .expect("probe retained header")
        );
        let (_, namespace) =
            pending::header_for_checkpoint(&conn, "opaque-repository", &checkpoint_id)
                .await
                .expect("read quarantined header")
                .expect("header retained");
        assert_eq!(
            namespace,
            crate::internal::metadata::MetadataScope::AgentCaptureQuarantine
        );

        let other_checkpoint = uuid::Uuid::new_v4().to_string();
        let foreign_worktree_header = header.replace(&checkpoint_id, &other_checkpoint).replace(
            "\"worktree_id\":\"\"",
            "\"worktree_id\":\"foreign-worktree\"",
        );
        crate::internal::metadata::MetadataKv::set_with_conn(
            &conn,
            crate::internal::metadata::MetadataScope::AgentCapturePending,
            "opaque-repository",
            &other_checkpoint,
            &foreign_worktree_header,
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("insert foreign-worktree pending header");
        assert!(
            quarantine_stale_pending_artifact(&conn, &scope, &other_checkpoint)
                .await
                .is_err(),
            "a different worktree binding must fail closed"
        );
        assert!(
            pending::has_pending_header_for_checkpoint(
                &conn,
                "opaque-repository",
                &other_checkpoint
            )
            .await
            .expect("probe untouched foreign-worktree pending header")
        );
    }

    #[test]
    fn pending_source_is_reported_and_never_swept() {
        let (detail, plan) = diagnose_pending_finalizer(false, false, false, false, false, false);
        assert!(detail.contains("pending_source"));
        assert!(detail.contains("cannot reopen provider sources"));
        assert_eq!(plan, FinalizerRepairKind::Manual);
        // The integration fixture drives real stop/resume; pin that its
        // superseded diagnosis takes precedence over every old-CAS/budget flag.
        for durable in [false, true] {
            for exhausted in [false, true] {
                for artifact in [false, true] {
                    let (detail, plan) = diagnose_pending_finalizer(
                        true, durable, exhausted, artifact, artifact, false,
                    );
                    assert!(detail.contains("superseded"));
                    assert_eq!(
                        plan,
                        if artifact {
                            FinalizerRepairKind::QuarantineStaleArtifact
                        } else {
                            FinalizerRepairKind::Manual
                        }
                    );
                }
            }
        }
        let (detail, plan) = diagnose_pending_finalizer(false, false, false, true, false, false);
        assert!(detail.contains("parked in quarantine"));
        assert_eq!(plan, FinalizerRepairKind::Manual);
        let (detail, plan) = diagnose_pending_finalizer(false, false, true, true, false, true);
        assert!(detail.contains("already consumed"));
        assert_eq!(plan, FinalizerRepairKind::Manual);
        let (detail, plan) = diagnose_pending_finalizer(false, false, true, true, false, false);
        assert!(detail.contains("manual recovery is required"));
        assert_eq!(plan, FinalizerRepairKind::QuarantineExhausted);
    }

    #[test]
    fn import_index_repair_pause_is_an_in_process_test_control() {
        let (reached_sender, reached_receiver) = std::sync::mpsc::channel();
        let (resume_sender, resume_receiver) = std::sync::mpsc::channel();
        let _reset = test_support::install(test_support::TestPause {
            reached: reached_sender,
            resume: resume_receiver,
        });
        let worker = std::thread::spawn(import_index_repair_test_pause_after_lock);
        reached_receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("test pause reports reaching the repair lock");
        resume_sender
            .send(())
            .expect("test pause waiter remains available");
        worker
            .join()
            .expect("test pause worker does not panic")
            .expect("test pause resumes after the in-process signal");
    }

    #[tokio::test]
    async fn final_scope_fence_rolls_back_object_index_repair_write() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open doctor test database");
        for statement in [
            "CREATE TABLE config_kv (id TEXT PRIMARY KEY)",
            "CREATE TABLE workspace_record (
                workspace_id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                lease_fence INTEGER NOT NULL,
                state TEXT NOT NULL,
                lease_owner TEXT,
                lease_expires_at INTEGER
            )",
            "CREATE TABLE object_index (
                o_id TEXT NOT NULL,
                o_type TEXT NOT NULL,
                o_size INTEGER NOT NULL,
                repo_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                is_synced INTEGER NOT NULL
            )",
            "INSERT INTO workspace_record (
                workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
             ) VALUES ('workspace-a', 'repo-a', 1, 'active', 'doctor-test-owner',
                       unixepoch('now') * 1000 + 60000)",
            "CREATE TRIGGER expire_doctor_scope_after_object_index_insert
             AFTER INSERT ON object_index
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'workspace-a';
             END",
        ] {
            conn.execute_unprepared(statement)
                .await
                .expect("create doctor scope-fence test fixture");
        }
        let scope = CaptureScope {
            repo_id: "repo-a".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("workspace-a".to_string()),
            workspace_fence: Some(1),
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin object-index repair transaction");
        scope
            .assert_workspace_fence_live(&txn)
            .await
            .expect("entry scope fence is live");
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced)
             VALUES ('object-a', 'blob', 1, 'repo-a', 1, 0)",
            [],
        ))
        .await
        .expect("write repaired object-index row");

        let error = commit_session_object_index_repair(txn, &scope)
            .await
            .expect_err("post-write expiry rejects the repair commit");
        assert!(
            format!("{error:#}").contains("capture workspace lease is no longer live"),
            "unexpected scope-fence error: {error:#}"
        );
        let count: i64 = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM object_index",
                [],
            ))
            .await
            .expect("read object-index row count")
            .expect("object-index count row")
            .try_get_by("count")
            .expect("decode object-index row count");
        assert_eq!(
            count, 0,
            "expired lease rolls back repaired object-index row"
        );
    }

    #[tokio::test]
    async fn object_index_repair_rolls_back_all_rows_when_second_dml_fails() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open doctor object-index repair test database");
        for statement in [
            "CREATE TABLE config_kv (id TEXT PRIMARY KEY)",
            "CREATE TABLE object_index (
                o_id TEXT NOT NULL,
                o_type TEXT NOT NULL,
                o_size INTEGER NOT NULL,
                repo_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                is_synced INTEGER NOT NULL
            )",
            "CREATE TRIGGER reject_second_doctor_object_index_write
             BEFORE INSERT ON object_index
             WHEN NEW.o_id = 'object-b'
             BEGIN
                 SELECT RAISE(FAIL, 'injected second object-index DML failure');
             END",
        ] {
            conn.execute_unprepared(statement)
                .await
                .expect("create doctor object-index repair fixture");
        }

        let error = repair_object_index_rows_atomically(
            &conn,
            &[
                ("object-a".to_string(), "blob".to_string(), 1),
                ("object-b".to_string(), "blob".to_string(), 2),
            ],
            &[],
            "repo-a",
        )
        .await
        .expect_err("injected second object-index DML failure rejects the repair plan");
        assert!(
            format!("{error:#}").contains("insert checkpoint object index row object-b"),
            "unexpected object-index repair error: {error:#}"
        );

        let count: i64 = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM object_index",
                [],
            ))
            .await
            .expect("read object-index row count after failed repair")
            .expect("object-index count row after failed repair")
            .try_get_by("count")
            .expect("decode object-index row count after failed repair");
        assert_eq!(
            count, 0,
            "a later object-index DML failure rolls back every earlier repair write"
        );
    }

    async fn scoped_catalog_repair_fixture() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open doctor catalog-repair test database");
        for statement in [
            "CREATE TABLE config_kv (id TEXT PRIMARY KEY)",
            "CREATE TABLE workspace_record (
                workspace_id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                lease_fence INTEGER NOT NULL,
                state TEXT NOT NULL,
                lease_owner TEXT,
                lease_expires_at INTEGER NOT NULL
            )",
            "CREATE TABLE agent_session (
                session_id TEXT PRIMARY KEY,
                scope_state TEXT NOT NULL,
                repo_id TEXT,
                worktree_id TEXT,
                workspace_id TEXT,
                workspace_fence INTEGER
            )",
            "CREATE TABLE agent_checkpoint (
                checkpoint_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                parent_checkpoint_id TEXT,
                scope TEXT NOT NULL,
                parent_commit TEXT,
                tree_oid TEXT NOT NULL,
                metadata_blob_oid TEXT NOT NULL,
                traces_commit TEXT NOT NULL,
                tool_use_id TEXT,
                subagent_session_id TEXT,
                description TEXT,
                created_at INTEGER NOT NULL,
                sync_revision INTEGER NOT NULL DEFAULT 0
            )",
            "INSERT INTO workspace_record (
                workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
             ) VALUES ('workspace-a', 'repo-a', 1, 'active', 'doctor-test-owner',
                       unixepoch('now') * 1000 + 60000)",
            "INSERT INTO agent_session (
                session_id, scope_state, repo_id, worktree_id, workspace_id, workspace_fence
             ) VALUES ('session-scoped', 'scoped', 'repo-a', '', 'workspace-a', 1)",
        ] {
            conn.execute_unprepared(statement)
                .await
                .expect("create doctor scoped catalog-repair fixture");
        }
        conn
    }

    async fn catalog_checkpoint_count(conn: &DatabaseConnection) -> i64 {
        conn.query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM agent_checkpoint",
            [],
        ))
        .await
        .expect("read checkpoint count")
        .expect("checkpoint count row")
        .try_get_by("count")
        .expect("decode checkpoint count")
    }

    #[tokio::test]
    async fn scoped_doctor_insert_rolls_back_after_post_insert_scope_expiry() {
        let conn = scoped_catalog_repair_fixture().await;
        conn.execute_unprepared(
            "CREATE TRIGGER expire_doctor_scope_after_checkpoint_insert
             AFTER INSERT ON agent_checkpoint
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'workspace-a';
             END",
        )
        .await
        .expect("install post-insert scope expiry trigger");
        let row = AgentCheckpointRow {
            checkpoint_id: "checkpoint-insert-expiry",
            session_id: "session-scoped",
            parent_commit: None,
            tree_oid: "tree-new",
            metadata_blob_oid: "metadata-new",
            traces_commit: "traces-new",
            created_at: 1,
        };

        let error = repair_insert_agent_checkpoint_catalog_row(&conn, &row)
            .await
            .expect_err("post-insert expiry must reject scoped doctor repair");
        assert!(
            format!("{error:#}").contains("capture workspace lease is no longer live"),
            "unexpected scope-fence error: {error:#}"
        );
        assert_eq!(
            catalog_checkpoint_count(&conn).await,
            0,
            "expired scope rolls the catalog insertion back"
        );

        let mut finding = CheckpointFinding {
            inconsistency_type: CLASS_MISSING_CATALOG_ROW.to_string(),
            checkpoint_id: row.checkpoint_id.to_string(),
            detail: "missing catalog row".to_string(),
            repaired: false,
            manual_required: false,
        };
        mark_catalog_repair_failed(&mut finding);
        assert!(
            !finding.repaired,
            "a rejected repair is never reported repaired"
        );
        assert!(
            finding.manual_required,
            "a scope-fenced repair remains actionable/manual for the operator"
        );
    }

    #[tokio::test]
    async fn scoped_doctor_update_rolls_back_after_post_update_scope_expiry() {
        let conn = scoped_catalog_repair_fixture().await;
        conn.execute_unprepared(
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at, sync_revision
             ) VALUES (
                'checkpoint-update-expiry', 'session-scoped', 'committed', NULL,
                'tree-old', 'metadata-old', 'traces-old', 1, 0
             )",
        )
        .await
        .expect("insert stale checkpoint catalog row");
        conn.execute_unprepared(
            "CREATE TRIGGER expire_doctor_scope_after_checkpoint_update
             AFTER UPDATE ON agent_checkpoint
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'workspace-a';
             END",
        )
        .await
        .expect("install post-update scope expiry trigger");

        let error = repair_update_checkpoint_catalog_row(
            &conn,
            "checkpoint-update-expiry",
            "tree-new",
            "metadata-new",
            "traces-new",
        )
        .await
        .expect_err("post-update expiry must reject scoped doctor repair");
        assert!(
            format!("{error:#}").contains("capture workspace lease is no longer live"),
            "unexpected scope-fence error: {error:#}"
        );
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT tree_oid, metadata_blob_oid, traces_commit, sync_revision
                 FROM agent_checkpoint WHERE checkpoint_id = 'checkpoint-update-expiry'",
                [],
            ))
            .await
            .expect("read stale checkpoint after rejected repair")
            .expect("stale checkpoint remains present");
        let tree_oid: String = row.try_get_by("tree_oid").expect("decode old tree");
        let metadata_blob_oid: String = row
            .try_get_by("metadata_blob_oid")
            .expect("decode old metadata");
        let traces_commit: String = row
            .try_get_by("traces_commit")
            .expect("decode old traces commit");
        let sync_revision: i64 = row
            .try_get_by("sync_revision")
            .expect("decode old sync revision");
        assert_eq!(
            (
                tree_oid.as_str(),
                metadata_blob_oid.as_str(),
                traces_commit.as_str(),
                sync_revision
            ),
            ("tree-old", "metadata-old", "traces-old", 0),
            "expired scope rolls the entire catalog update back"
        );
    }

    #[tokio::test]
    async fn legacy_unknown_doctor_catalog_insert_remains_repairable() {
        let conn = scoped_catalog_repair_fixture().await;
        conn.execute_unprepared(
            "UPDATE agent_session
             SET scope_state = 'legacy_unknown', repo_id = NULL, worktree_id = NULL,
                 workspace_id = NULL, workspace_fence = NULL
             WHERE session_id = 'session-scoped'",
        )
        .await
        .expect("make fixture an explicit legacy catalog row");
        conn.execute_unprepared(
            "UPDATE workspace_record SET lease_expires_at = 0 WHERE workspace_id = 'workspace-a'",
        )
        .await
        .expect("expire irrelevant workspace record");
        let row = AgentCheckpointRow {
            checkpoint_id: "checkpoint-legacy",
            session_id: "session-scoped",
            parent_commit: None,
            tree_oid: "tree-legacy",
            metadata_blob_oid: "metadata-legacy",
            traces_commit: "traces-legacy",
            created_at: 1,
        };

        assert!(
            repair_insert_agent_checkpoint_catalog_row(&conn, &row)
                .await
                .expect("explicit legacy catalog repair remains compatible")
        );
        assert_eq!(catalog_checkpoint_count(&conn).await, 1);
    }

    #[test]
    fn libra_trailers_parse_parent_commit_and_scope() {
        let message = "traces: committed checkpoint abc\n\n\
                       Libra-Session: claude__s1\n\
                       Libra-Agent: claude_code\n\
                       Libra-Parent-Commit: 0123456789012345678901234567890123456789\n\
                       Libra-Checkpoint-ID: abc\n\
                       Libra-Scope: committed\n";
        let trailers = parse_libra_trailers(message);
        assert_eq!(
            trailers.parent_commit.as_deref(),
            Some("0123456789012345678901234567890123456789")
        );
        assert_eq!(trailers.scope.as_deref(), Some("committed"));
    }

    #[test]
    fn libra_trailers_tolerate_missing_parent_commit() {
        let message = "traces: committed checkpoint abc\n\n\
                       Libra-Session: claude__s1\n\
                       Libra-Scope: committed\n";
        let trailers = parse_libra_trailers(message);
        assert_eq!(trailers.parent_commit, None);
        assert_eq!(trailers.scope.as_deref(), Some("committed"));
    }

    /// Both metadata generations parse into the doctor probe: v1
    /// (pre-AG-20, no `model`) and v2 (AG-20, adds `model`).
    #[test]
    fn metadata_probe_parses_v1_and_v2_shapes() {
        let v1 = serde_json::json!({
            "schema_version": 1,
            "checkpoint_id": "85ae75d2-4c53-465a-b890-a9f861a50cc7",
            "session_id": "claude__fixture-v1-claude",
            "agent_kind": "claude_code",
            "scope": "committed",
            "provider_session_id": "fixture-v1-claude",
            "working_dir": "/tmp/x",
            "redaction_report": {},
            "created_at": 1783206712
        });
        let parsed: CheckpointMetadataProbe =
            serde_json::from_value(v1).expect("v1 metadata parses");
        assert_eq!(parsed.session_id, "claude__fixture-v1-claude");
        assert_eq!(parsed.created_at, 1783206712);
        assert_eq!(parsed.scope.as_deref(), Some("committed"));

        let v2 = serde_json::json!({
            "schema_version": 2,
            "checkpoint_id": "b",
            "session_id": "claude__s2",
            "agent_kind": "claude_code",
            "scope": "committed",
            "provider_session_id": "s2",
            "working_dir": "/tmp/x",
            "model": "unknown",
            "redaction_report": {},
            "created_at": 42
        });
        let parsed: CheckpointMetadataProbe =
            serde_json::from_value(v2).expect("v2 metadata parses");
        assert_eq!(parsed.session_id, "claude__s2");
        assert_eq!(parsed.created_at, 42);
    }

    /// The manifest sweep understands both transcript shapes while deriving
    /// only fixed labels from entry roles, never caller-shaped paths.
    #[test]
    fn manifest_declared_blobs_covers_single_and_chunked_entries() {
        let metadata_oid = "a".repeat(40);
        let lifecycle_oid = "b".repeat(40);
        let transcript_part_one_oid = "c".repeat(40);
        let transcript_part_two_oid = "d".repeat(40);
        let redaction_oid = "e".repeat(40);
        let content_hash_oid = "f".repeat(40);
        let manifest = serde_json::json!({
            "schema_version": 1,
            "checkpoint_id": "abc",
            "entries": {
                "metadata": { "path": "metadata.json", "oid": metadata_oid, "byte_len": 100 },
                "lifecycle_events": {
                    "path": "events/lifecycle.jsonl", "oid": lifecycle_oid, "byte_len": 200
                },
                "transcript": {
                    "path": "transcript/claude_code.jsonl",
                    "byte_len": 700,
                    "chunked": true,
                    "parts": [
                        {
                            "path": "transcript/claude_code.jsonl.001",
                            "oid": transcript_part_one_oid,
                            "byte_len": 400
                        },
                        {
                            "path": "transcript/claude_code.jsonl.002",
                            "oid": transcript_part_two_oid,
                            "byte_len": 300
                        }
                    ]
                },
                "redaction_report": {
                    "path": "redaction_report.json", "oid": redaction_oid, "byte_len": 50
                },
                "content_hash": { "path": "content_hash.txt", "oid": content_hash_oid, "byte_len": 71 }
            }
        });
        let parsed = manifest_declared_blobs(&manifest);
        assert!(!parsed.over_limit, "ordinary manifest stays within the cap");
        let mut declared = parsed
            .declarations
            .into_iter()
            .map(|entry| (entry.label, entry.oid, entry.o_type))
            .collect::<Vec<_>>();
        declared.sort_by(|left, right| left.0.cmp(&right.0));
        // Chunked transcript: the top-level entry has NO `oid` (only
        // `parts`), so exactly the per-chunk OIDs surface. `byte_len` is
        // deliberately ignored; class 3 streams the held object descriptor
        // and verifies its content-addressed identity before writing a size.
        assert_eq!(
            declared,
            vec![
                (
                    "manifest-declared content hash".to_string(),
                    Some("f".repeat(40)),
                    "blob",
                ),
                (
                    "manifest-declared lifecycle events".to_string(),
                    Some("b".repeat(40)),
                    "blob",
                ),
                (
                    "manifest-declared metadata".to_string(),
                    Some("a".repeat(40)),
                    "blob",
                ),
                (
                    "manifest-declared redaction report".to_string(),
                    Some("e".repeat(40)),
                    "blob",
                ),
                (
                    "manifest-declared transcript part 1".to_string(),
                    Some("c".repeat(40)),
                    "agent_transcript",
                ),
                (
                    "manifest-declared transcript part 2".to_string(),
                    Some("d".repeat(40)),
                    "agent_transcript",
                ),
            ]
        );

        // Single-file transcript (small): plain `oid` is cross-checked to
        // the tree, but its declared byte_len never reaches object_index.
        let single_oid = "c".repeat(40);
        let single = serde_json::json!({
            "entries": {
                "transcript": {
                    "path": "transcript/claude_code.jsonl", "oid": single_oid, "byte_len": 42
                }
            }
        });
        assert_eq!(
            manifest_declared_blobs(&single),
            ManifestDeclarations {
                declarations: vec![ManifestDeclaredBlob {
                    label: "manifest-declared transcript".to_string(),
                    role: Some(ManifestRole::Transcript),
                    oid: Some("c".repeat(40)),
                    o_type: "agent_transcript",
                }],
                over_limit: false,
            }
        );

        // Legacy/corrupt manifests without byte_len still yield the OIDs;
        // size comes from descriptor-pinned streaming validation instead.
        let no_len_oid = "a".repeat(40);
        let no_len = serde_json::json!({
            "entries": {
                "metadata": { "path": "metadata.json", "oid": no_len_oid }
            }
        });
        assert_eq!(
            manifest_declared_blobs(&no_len),
            ManifestDeclarations {
                declarations: vec![ManifestDeclaredBlob {
                    label: "manifest-declared metadata".to_string(),
                    role: Some(ManifestRole::Metadata),
                    oid: Some("a".repeat(40)),
                    o_type: "blob",
                }],
                over_limit: false,
            }
        );

        let damaged = serde_json::json!({
            "entries": {
                "transcript": {
                    "path": "/private/manifest-path-secret",
                    "oid": "manifest-oid-secret"
                }
            }
        });
        assert_eq!(
            manifest_declared_blobs(&damaged),
            ManifestDeclarations {
                declarations: vec![ManifestDeclaredBlob {
                    label: "manifest-declared transcript".to_string(),
                    role: Some(ManifestRole::Transcript),
                    oid: None,
                    o_type: "agent_transcript",
                }],
                over_limit: false,
            }
        );
    }

    #[test]
    fn manifest_declaration_limit_fails_closed_without_retaining_all_parts() {
        let part = serde_json::json!({ "oid": "a".repeat(40), "byte_len": 1 });
        let manifest = serde_json::json!({
            "entries": {
                "transcript": {
                    "parts": vec![part; DOCTOR_E4_MANIFEST_DECLARATION_CAP + 1]
                }
            }
        });
        let declarations = manifest_declared_blobs(&manifest);
        assert!(
            declarations.over_limit,
            "oversized parts list must be manual-only"
        );
        assert!(
            declarations.declarations.len() <= DOCTOR_E4_MANIFEST_DECLARATION_CAP,
            "bounded parser must not retain every malicious declaration"
        );
    }

    #[test]
    fn e4_sweep_global_object_limit_stops_before_unbounded_fan_out() {
        let mut sweep = E4Sweep::default();
        let mut seen = BTreeSet::new();
        for index in 0..=DOCTOR_E4_OBJECT_CAP {
            sweep.record(
                &mut seen,
                "unrecognized checkpoint sidecar",
                &format!("{index:040x}"),
                "blob",
                None,
                true,
            );
        }
        assert!(
            sweep.entry_limit_hit,
            "global sweep budget must stop the walk"
        );
        assert_eq!(
            sweep.present.len(),
            DOCTOR_E4_OBJECT_CAP,
            "the sweep must not retain an unbounded reachability set"
        );
        assert!(
            sweep
                .missing
                .iter()
                .any(|detail| detail.contains("doctor entry limit")),
            "cap hit must become a fixed manual-review finding: {sweep:?}"
        );
    }

    /// The per-object cap is not sufficient on its own: many valid small
    /// objects could otherwise make doctor stream an unbounded aggregate.
    #[cfg(unix)]
    #[test]
    fn object_index_validation_budget_is_cumulative_across_objects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_path = temp.path();
        std::fs::create_dir_all(repo_path.join("objects")).expect("objects directory");
        let first = crate::utils::object::write_git_object(repo_path, "blob", b"abc")
            .expect("write first loose object");
        let second = crate::utils::object::write_git_object(repo_path, "blob", b"def")
            .expect("write second loose object");
        let reader = ObjectReader {
            repo_path: repo_path.to_path_buf(),
        };
        let mut budget = ObjectIndexValidationBudget { remaining_bytes: 5 };

        assert_eq!(
            budget
                .validate(&reader, &first, "blob")
                .expect("first object fits aggregate budget"),
            3
        );
        assert_eq!(budget.remaining_bytes, 2);
        assert!(
            budget.validate(&reader, &second, "blob").is_err(),
            "the second 3-byte object must not exceed the remaining 2-byte budget"
        );
        assert_eq!(
            budget.remaining_bytes, 2,
            "a rejected validation must not consume budget or allow partial accounting"
        );
    }

    /// RC-31: only existing residue paths are reported, as repository-relative
    /// names; an empty storage root reports nothing.
    #[test]
    fn legacy_code_residue_paths_reports_existing_relative_paths_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = temp.path().join(".libra");
        assert!(legacy_code_residue_paths(&storage).is_empty());

        std::fs::create_dir_all(storage.join("sessions").join("code")).expect("sessions/code");
        std::fs::create_dir_all(storage.join("code")).expect("code");
        assert_eq!(
            legacy_code_residue_paths(&storage),
            vec![
                ".libra/sessions/code".to_string(),
                ".libra/code".to_string()
            ]
        );
    }

    /// The findings scanner must list the same directory object it opened
    /// with the no-follow walk. Re-resolving the pathname here would let a
    /// concurrent replacement redirect an otherwise descriptor-safe repair
    /// into an attacker-controlled tree.
    #[cfg(unix)]
    #[test]
    fn agent_run_listing_stays_pinned_when_storage_path_is_replaced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runs_root = temp.path().join("agent-runs");
        std::fs::create_dir_all(runs_root.join("original-run")).expect("create original run");
        let pinned = crate::utils::object::open_directory_tree_no_follow(&runs_root)
            .expect("pin original agent-runs directory");

        let displaced = temp.path().join("agent-runs-displaced");
        std::fs::rename(&runs_root, &displaced).expect("displace original root");
        std::fs::create_dir_all(runs_root.join("attacker-run")).expect("create replacement root");

        let names = pinned
            .try_clone()
            .and_then(crate::utils::beneath::read_dir_fd)
            .expect("list held root descriptor")
            .map(|entry| entry.expect("read held root entry").name)
            .collect::<Vec<_>>();
        assert!(
            names.iter().any(|name| name == "original-run"),
            "pinned listing must retain the original directory: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "attacker-run"),
            "pinned listing must not follow the replacement directory: {names:?}"
        );
    }
}
