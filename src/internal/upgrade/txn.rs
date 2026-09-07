//! Crash-safe install transaction and recovery (plan-20260714 §A.7 Phase B +
//! Txn 恢复).
//!
//! The install is a durable state machine journalled to
//! `.libra-upgrade-txn.json` inside the install directory. Every step writes
//! its intent BEFORE the corresponding filesystem mutation, so a crash at any
//! point leaves the directory in a state the recovery table can classify and
//! drive to a terminal outcome (committed, or rolled back to the previous
//! target / uninstalled). All file operations are fd-relative and no-follow
//! via [`InstallDir`]; the caller holds the §A.5 lock across the whole
//! transaction and its recovery.
//!
//! The post-install self-check ("post-probe") is injected as a callback so
//! recovery is exhaustively testable by constructing each intermediate
//! on-disk state directly, without spawning a real candidate binary (§A.7:
//! `CandidateInstalled` must re-probe on recovery). The live probe wiring
//! lands in the probe/orchestration slice.

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use super::{
    lock::{EntryKind, InstallDir, InstallDirError},
    marker::{InstallMarker, MARKER_FILE_NAME, TARGET_BINARY_NAME, write_marker},
    state::{StateStoreError, UpgradeState, merge_acceptance_floors, read_state, write_state},
};

/// Transaction journal file name (fd-relative, `0600`).
pub const TXN_FILE_NAME: &str = ".libra-upgrade-txn.json";
/// Candidate (newly downloaded, verified) binary name during a transaction.
pub const CANDIDATE_NAME: &str = ".libra-upgrade-candidate";
/// Backup of the previous target binary during a Present-branch transaction.
pub const BACKUP_NAME: &str = ".libra-upgrade-backup";

/// The previous target at transaction start (§A.7 Phase B).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum OldTarget {
    /// Fresh install: no target existed.
    Absent,
    /// Upgrade: a target existed; its hash and marker are snapshotted so a
    /// rollback restores byte-for-byte.
    Present {
        /// Lowercase 64-hex sha256 of the previous target.
        hash: String,
        /// The previous official marker, if any (restored on rollback).
        marker_snapshot: Option<InstallMarker>,
    },
}

/// Durable transaction state (§A.7 state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxnState {
    Prepared,
    BackupDurable,
    CandidateInstalled,
    PostProbePassed,
    RollbackIntent,
    AbortAbsentIntent,
    Committed,
}

/// The journalled transaction record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Txn {
    pub schema_version: u32,
    pub state: TxnState,
    pub old_target: OldTarget,
    /// Version being installed (canonical `X.Y.Z`).
    pub new_version: String,
    /// Lowercase 64-hex sha256 of the candidate/new target.
    pub new_hash: String,
    /// Marker to record on commit.
    pub marker: InstallMarker,
    /// Anti-rollback state to persist on commit (already validated).
    pub new_state: UpgradeState,
}

/// Outcome of a transaction or its recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnOutcome {
    /// The new target is installed and committed.
    Installed,
    /// The previous target was restored (post-probe failed on an upgrade).
    RolledBack,
    /// A fresh install was aborted; nothing is installed.
    AbortedAbsent,
    /// Nothing needed doing (already committed / already clean).
    NoOp,
}

/// Transaction / recovery failures.
#[derive(Debug, thiserror::Error)]
pub enum TxnError {
    #[error(transparent)]
    Dir(#[from] InstallDirError),
    #[error("cannot (de)serialize the upgrade transaction: {0}")]
    Serde(String),
    #[error("failed to persist anti-rollback state during commit: {0}")]
    State(String),
    #[error("failed to persist the install marker during commit: {0}")]
    Marker(String),
    #[error(
        "upgrade transaction is unrecoverable: state {state:?} does not match the on-disk \
         layout ({detail}); the install directory needs manual inspection"
    )]
    FatalRecovery { state: TxnState, detail: String },
}

/// Post-install self-check: `Ok(true)` when the installed target is healthy.
/// Injected so recovery is testable without spawning a real binary.
pub type PostProbe<'a> = dyn Fn(&InstallDir) -> Result<bool, TxnError> + 'a;

/// Commit fence (§A.6 policy): consulted AFTER the post-install probe passes
/// and BEFORE the commit writes anything. `Ok(Some(guard))` proceeds with the
/// guard held through the durable `PostProbePassed` journal write ONLY — the
/// commit tail (state/marker writes, cleanup) runs after the guard drops,
/// under the same contract crash recovery grants that journaled state — so a
/// concurrent control write cannot land between the final policy check and
/// the journaled decision; `Ok(None)` VETOES the commit and the
/// transaction rolls back exactly like a failed probe. A `None` fence
/// (recovery, tests) does NOT commit unconditionally: it goes through
/// [`default_fence`] — the same bounded floors-lock acquisition and
/// persisted-floor comparison — so a superseding control decision vetoes
/// recovery commits too.
pub type CommitFence<'a> =
    dyn Fn(&InstallDir) -> Result<Option<super::lock::UpgradeLock>, TxnError> + 'a;

fn load_txn(dir: &InstallDir) -> Result<Option<Txn>, TxnError> {
    let Some(bytes) = dir.read_file(TXN_FILE_NAME)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| TxnError::Serde(e.to_string()))
}

fn store_txn(dir: &InstallDir, txn: &Txn) -> Result<(), TxnError> {
    let mut bytes = serde_json::to_vec_pretty(txn).map_err(|e| TxnError::Serde(e.to_string()))?;
    bytes.push(b'\n');
    dir.write_file_atomic(TXN_FILE_NAME, &bytes, 0o600)?;
    Ok(())
}

/// Hash of an entry inside the directory, or `None` when absent. Non-regular
/// entries are treated as a fatal layout anomaly by the callers that care.
fn entry_hash(dir: &InstallDir, name: &str) -> Result<Option<String>, TxnError> {
    match dir.stat_entry(name)? {
        Some(EntryKind::Regular { .. }) => {
            let bytes = dir.read_file(name)?.unwrap_or_default();
            Ok(Some(hex::encode(sha2::Sha256::digest(&bytes))))
        }
        Some(_) => Ok(None),
        None => Ok(None),
    }
}

/// Observed identities of the three transaction files.
struct Layout {
    target: Option<String>,
    candidate: Option<String>,
    backup: Option<String>,
}

fn observe(dir: &InstallDir) -> Result<Layout, TxnError> {
    Ok(Layout {
        target: entry_hash(dir, TARGET_BINARY_NAME)?,
        candidate: entry_hash(dir, CANDIDATE_NAME)?,
        backup: entry_hash(dir, BACKUP_NAME)?,
    })
}

/// Commit: persist marker + anti-rollback state, verify identity, remove
/// backup/candidate, fsync, then delete the txn LAST (§A.7).
fn finish_commit(dir: &InstallDir, txn: &Txn) -> Result<TxnOutcome, TxnError> {
    // Callers hold the upgrade lock through commit/recovery, so this state
    // write cannot overwrite a newer accepted anti-rollback floor.
    write_state(dir, &txn.new_state).map_err(|e| TxnError::State(e.to_string()))?;
    write_marker(dir, &txn.marker).map_err(|e| TxnError::Marker(e.to_string()))?;
    // Identity re-check: the committed target must be the new hash.
    let layout = observe(dir)?;
    if layout.target.as_deref() != Some(txn.new_hash.as_str()) {
        return Err(TxnError::FatalRecovery {
            state: TxnState::Committed,
            detail: "committed target hash does not match the transaction".into(),
        });
    }
    dir.remove_file(BACKUP_NAME)?;
    dir.remove_file(CANDIDATE_NAME)?;
    dir.fsync_dir()?;
    dir.remove_file(TXN_FILE_NAME)?;
    dir.fsync_dir()?;
    Ok(TxnOutcome::Installed)
}

/// Persist the accepted manifest's monotone anti-rollback floors even though
/// the install is being undone: the manifest itself WAS verified, so a later
/// manifest signed below its `min_key_generation` must stay rejected. Runs
/// under the caller-held §A.5 lock in both the live and recovery paths.
fn persist_acceptance_floors_on_failure(dir: &InstallDir, txn: &Txn) -> Result<(), TxnError> {
    let current = match read_state(dir) {
        Ok(state) => state,
        // A corrupt state file cannot be allowed to drop the accepted floors:
        // rebuild from the transaction's validated snapshot (floors only ever
        // advance from the default). Unreadable (I/O) state stays an error —
        // overwriting a state we could not read might lose a higher floor.
        Err(StateStoreError::Corrupt { .. }) => UpgradeState::default(),
        Err(e) => return Err(TxnError::State(e.to_string())),
    };
    write_state(dir, &merge_acceptance_floors(&current, &txn.new_state))
        .map_err(|e| TxnError::State(e.to_string()))
}

/// Roll back an upgrade: restore the backup over the target, then clean up
/// (§A.7 RollbackIntent).
fn finish_rollback(dir: &InstallDir, txn: &Txn) -> Result<TxnOutcome, TxnError> {
    let OldTarget::Present {
        hash,
        marker_snapshot,
    } = &txn.old_target
    else {
        return Err(TxnError::FatalRecovery {
            state: TxnState::RollbackIntent,
            detail: "rollback intent on a fresh (Absent) install".into(),
        });
    };
    persist_acceptance_floors_on_failure(dir, txn)?;
    let layout = observe(dir)?;
    // Restore backup → target unless the old target is already in place.
    if layout.target.as_deref() != Some(hash.as_str()) {
        if layout.backup.as_deref() != Some(hash.as_str()) {
            return Err(TxnError::FatalRecovery {
                state: TxnState::RollbackIntent,
                detail: "neither target nor backup carries the previous hash".into(),
            });
        }
        dir.rename_entry(BACKUP_NAME, TARGET_BINARY_NAME)?;
    }
    // Restore or clear the previous marker snapshot.
    match marker_snapshot {
        Some(marker) => write_marker(dir, marker).map_err(|e| TxnError::Marker(e.to_string()))?,
        None => {
            dir.remove_file(MARKER_FILE_NAME)?;
        }
    }
    let final_target = observe(dir)?.target;
    if final_target.as_deref() != Some(hash.as_str()) {
        return Err(TxnError::FatalRecovery {
            state: TxnState::RollbackIntent,
            detail: "restored target hash does not match the previous target".into(),
        });
    }
    dir.remove_file(BACKUP_NAME)?;
    dir.remove_file(CANDIDATE_NAME)?;
    dir.fsync_dir()?;
    dir.remove_file(TXN_FILE_NAME)?;
    dir.fsync_dir()?;
    Ok(TxnOutcome::RolledBack)
}

/// Abort a fresh install: remove the new target if present, then clean up
/// (§A.7 AbortAbsentIntent).
fn finish_abort_absent(dir: &InstallDir, txn: &Txn) -> Result<TxnOutcome, TxnError> {
    persist_acceptance_floors_on_failure(dir, txn)?;
    let layout = observe(dir)?;
    if layout.target.as_deref() == Some(txn.new_hash.as_str()) {
        dir.remove_file(TARGET_BINARY_NAME)?;
    }
    dir.remove_file(CANDIDATE_NAME)?;
    dir.remove_file(MARKER_FILE_NAME)?;
    dir.fsync_dir()?;
    dir.remove_file(TXN_FILE_NAME)?;
    dir.fsync_dir()?;
    Ok(TxnOutcome::AbortedAbsent)
}

/// Post-probe the installed target and branch to commit or rollback/abort
/// (§A.7 CandidateInstalled → …).
/// Bounded floors-lock acquisition + policy comparison shared by the
/// commit fence's default arm: 50 × 100 ms non-blocking probes (a stuck
/// holder must not hang recovery), then one state read under the guard.
/// `Ok(None)` = veto (lock unobtainable, or the transaction is superseded
/// by a higher persisted generation/control floor).
fn default_fence(
    dir: &InstallDir,
    txn_state: &UpgradeState,
) -> Result<Option<super::lock::UpgradeLock>, TxnError> {
    let mut guard = None;
    for _ in 0..50 {
        match dir.try_lock_floors() {
            Ok(Some(g)) => {
                guard = Some(g);
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => return Err(TxnError::State(format!("floors lock: {e}"))),
        }
    }
    let Some(guard) = guard else {
        return Ok(None);
    };
    let current = read_state(dir).map_err(|e| TxnError::State(e.to_string()))?;
    if current.generation_floor > txn_state.generation_floor
        || current.max_control_revision > txn_state.max_control_revision
    {
        return Ok(None);
    }
    Ok(Some(guard))
}

fn probe_and_resolve(
    dir: &InstallDir,
    txn: &mut Txn,
    post_probe: &PostProbe<'_>,
    commit_fence: Option<&CommitFence<'_>>,
) -> Result<TxnOutcome, TxnError> {
    if post_probe(dir)? {
        // Policy fence. The guard (when present) is held ONLY from the
        // final policy check through the durable `PostProbePassed` journal
        // write — one read + one journal fsync, microseconds of I/O — and
        // is dropped BEFORE the commit tail. Once `PostProbePassed` is
        // durable, committing without a fence is the same contract crash
        // recovery already has for that state.
        //
        // With no custom fence (recovery, tests) the bounded-LOCKED default check
        // still compares the persisted floors against the transaction: a
        // pause/revocation/rotation accepted while this transaction was in
        // flight vetoes the commit instead of resurrecting a superseded
        // plan. (Test fixtures start from default state, whose floors never
        // exceed a transaction's own, so they are unaffected.)
        // Both arms produce the same three-way result; the DEFAULT arm
        // (recovery, tests) is the bounded-locked policy check — never an
        // unconditional commit. A fence ERROR (lock/state unreadable) must
        // not strand the transaction in CandidateInstalled: roll back FIRST
        // so the previous binary is restored, then surface the error.
        let fence_result = match commit_fence {
            Some(fence) => fence(dir),
            None => default_fence(dir, &txn.new_state),
        };
        let vetoed = match fence_result {
            Ok(Some(guard)) => {
                txn.state = TxnState::PostProbePassed;
                store_txn(dir, txn)?;
                drop(guard);
                false
            }
            Ok(None) => true,
            Err(error) => {
                let undo = match txn.old_target {
                    OldTarget::Absent => {
                        txn.state = TxnState::AbortAbsentIntent;
                        store_txn(dir, txn)?;
                        finish_abort_absent(dir, txn)
                    }
                    OldTarget::Present { .. } => {
                        txn.state = TxnState::RollbackIntent;
                        store_txn(dir, txn)?;
                        finish_rollback(dir, txn)
                    }
                };
                undo?;
                return Err(error);
            }
        };
        if vetoed {
            // Vetoed: a newer control decision superseded this plan after
            // the probe. Undo exactly like a failed probe.
            return match txn.old_target {
                OldTarget::Absent => {
                    txn.state = TxnState::AbortAbsentIntent;
                    store_txn(dir, txn)?;
                    finish_abort_absent(dir, txn)
                }
                OldTarget::Present { .. } => {
                    txn.state = TxnState::RollbackIntent;
                    store_txn(dir, txn)?;
                    finish_rollback(dir, txn)
                }
            };
        }
        finish_commit(dir, txn)
    } else {
        match txn.old_target {
            OldTarget::Absent => {
                txn.state = TxnState::AbortAbsentIntent;
                store_txn(dir, txn)?;
                finish_abort_absent(dir, txn)
            }
            OldTarget::Present { .. } => {
                txn.state = TxnState::RollbackIntent;
                store_txn(dir, txn)?;
                finish_rollback(dir, txn)
            }
        }
    }
}

/// Recover an interrupted transaction (§A.7 decision table). Idempotent:
/// safe to call repeatedly; each intermediate on-disk layout maps to exactly
/// one action, and any layout inconsistent with the recorded state is
/// `FatalRecovery`.
pub fn recover(dir: &InstallDir, post_probe: &PostProbe<'_>) -> Result<TxnOutcome, TxnError> {
    let Some(mut txn) = load_txn(dir)? else {
        return Ok(TxnOutcome::NoOp);
    };
    let layout = observe(dir)?;
    let new = txn.new_hash.clone();
    let fatal = |detail: &str| TxnError::FatalRecovery {
        state: txn.state,
        detail: detail.to_string(),
    };

    match (txn.state, &txn.old_target) {
        (TxnState::Prepared, OldTarget::Absent) => {
            if layout.target.is_none() && layout.candidate.as_deref() == Some(new.as_str()) {
                persist_acceptance_floors_on_failure(dir, &txn)?;
                dir.remove_file(CANDIDATE_NAME)?;
                dir.fsync_dir()?;
                dir.remove_file(TXN_FILE_NAME)?;
                dir.fsync_dir()?;
                Ok(TxnOutcome::AbortedAbsent)
            } else if layout.target.as_deref() == Some(new.as_str()) && layout.candidate.is_none() {
                // rename landed but state was not yet advanced.
                txn.state = TxnState::CandidateInstalled;
                store_txn(dir, &txn)?;
                probe_and_resolve(dir, &mut txn, post_probe, None)
            } else {
                Err(fatal("Prepared/Absent layout unrecognized"))
            }
        }
        (TxnState::Prepared, OldTarget::Present { hash, .. }) => {
            let target_old = layout.target.as_deref() == Some(hash.as_str());
            let candidate_new = layout.candidate.as_deref() == Some(new.as_str());
            if target_old && candidate_new && layout.backup.is_none() {
                persist_acceptance_floors_on_failure(dir, &txn)?;
                dir.remove_file(CANDIDATE_NAME)?;
                dir.fsync_dir()?;
                dir.remove_file(TXN_FILE_NAME)?;
                dir.fsync_dir()?;
                Ok(TxnOutcome::NoOp)
            } else if target_old && candidate_new && layout.backup.as_deref() == Some(hash.as_str())
            {
                txn.state = TxnState::BackupDurable;
                store_txn(dir, &txn)?;
                continue_overwrite_from_backup_durable(dir, &mut txn, post_probe, None)
            } else {
                Err(fatal("Prepared/Present layout unrecognized"))
            }
        }
        (TxnState::BackupDurable, OldTarget::Present { hash, .. }) => {
            let candidate_new = layout.candidate.as_deref() == Some(new.as_str());
            let backup_old = layout.backup.as_deref() == Some(hash.as_str());
            if layout.target.as_deref() == Some(hash.as_str()) && candidate_new && backup_old {
                continue_overwrite_from_backup_durable(dir, &mut txn, post_probe, None)
            } else if layout.target.as_deref() == Some(new.as_str())
                && layout.candidate.is_none()
                && backup_old
            {
                txn.state = TxnState::CandidateInstalled;
                store_txn(dir, &txn)?;
                probe_and_resolve(dir, &mut txn, post_probe, None)
            } else {
                Err(fatal("BackupDurable layout unrecognized"))
            }
        }
        (TxnState::BackupDurable, OldTarget::Absent) => Err(fatal(
            "BackupDurable is only valid for an upgrade (Present)",
        )),
        (TxnState::CandidateInstalled, _) => {
            if layout.target.as_deref() != Some(new.as_str()) {
                return Err(fatal("CandidateInstalled but target is not the new hash"));
            }
            probe_and_resolve(dir, &mut txn, post_probe, None)
        }
        (TxnState::PostProbePassed, _) => {
            if layout.target.as_deref() != Some(new.as_str()) {
                return Err(fatal("PostProbePassed but target is not the new hash"));
            }
            finish_commit(dir, &txn)
        }
        (TxnState::AbortAbsentIntent, OldTarget::Absent) => finish_abort_absent(dir, &txn),
        (TxnState::AbortAbsentIntent, OldTarget::Present { .. }) => {
            Err(fatal("AbortAbsentIntent on a Present install"))
        }
        (TxnState::RollbackIntent, OldTarget::Present { .. }) => finish_rollback(dir, &txn),
        (TxnState::RollbackIntent, OldTarget::Absent) => {
            Err(fatal("RollbackIntent on a fresh (Absent) install"))
        }
        (TxnState::Committed, _) => finish_commit(dir, &txn),
    }
}

/// From `BackupDurable`: atomically overwrite the target with the candidate,
/// advance to `CandidateInstalled`, then probe (§A.7 rows).
fn continue_overwrite_from_backup_durable(
    dir: &InstallDir,
    txn: &mut Txn,
    post_probe: &PostProbe<'_>,
    commit_fence: Option<&CommitFence<'_>>,
) -> Result<TxnOutcome, TxnError> {
    dir.rename_entry(CANDIDATE_NAME, TARGET_BINARY_NAME)?;
    dir.fsync_dir()?;
    txn.state = TxnState::CandidateInstalled;
    store_txn(dir, txn)?;
    probe_and_resolve(dir, txn, post_probe, commit_fence)
}

/// Drive a fresh transaction to completion. The caller has already written
/// the verified candidate to [`CANDIDATE_NAME`] (via [`InstallDir`]) and
/// holds the §A.5 lock. `old_target` reflects the pre-install target.
// The transaction inputs are all independently sourced (§A.7); bundling
// them into a struct would only add ceremony at the two call sites.
#[allow(clippy::too_many_arguments)]
pub fn run_install(
    dir: &InstallDir,
    old_target: OldTarget,
    new_version: &str,
    new_hash: &str,
    marker: InstallMarker,
    new_state: UpgradeState,
    post_probe: &PostProbe<'_>,
    commit_fence: Option<&CommitFence<'_>>,
) -> Result<TxnOutcome, TxnError> {
    let mut txn = Txn {
        schema_version: 1,
        state: TxnState::Prepared,
        old_target,
        new_version: new_version.to_string(),
        new_hash: new_hash.to_string(),
        marker,
        new_state,
    };
    store_txn(dir, &txn)?;

    match &txn.old_target {
        OldTarget::Absent => {
            // Fresh install: no backup; rename candidate into place.
            dir.rename_entry(CANDIDATE_NAME, TARGET_BINARY_NAME)?;
            dir.fsync_dir()?;
            txn.state = TxnState::CandidateInstalled;
            store_txn(dir, &txn)?;
            probe_and_resolve(dir, &mut txn, post_probe, commit_fence)
        }
        OldTarget::Present { .. } => {
            // Upgrade: durable backup BEFORE overwrite.
            dir.rename_entry(TARGET_BINARY_NAME, BACKUP_NAME)?;
            dir.fsync_dir()?;
            txn.state = TxnState::BackupDurable;
            store_txn(dir, &txn)?;
            continue_overwrite_from_backup_durable(dir, &mut txn, post_probe, commit_fence)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::internal::upgrade::marker::{OFFICIAL_INSTALL_SOURCE, official_marker_for_target};

    fn dir() -> (tempfile::TempDir, InstallDir) {
        let guard = tempfile::tempdir().expect("test fixture operation should succeed");
        let path = guard
            .path()
            .canonicalize()
            .expect("test fixture operation should succeed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("test fixture operation should succeed");
        let d = InstallDir::open_validated(&path).expect("test fixture operation should succeed");
        (guard, d)
    }

    fn hash(bytes: &[u8]) -> String {
        hex::encode(sha2::Sha256::digest(bytes))
    }

    fn marker_for(version: &str, bytes: &[u8]) -> InstallMarker {
        InstallMarker {
            schema_version: 1,
            installed_at: "2026-07-17T00:00:00Z".into(),
            install_source: OFFICIAL_INSTALL_SOURCE.into(),
            platform: "darwin-arm64".into(),
            version: version.into(),
            sha256: hash(bytes),
            size: bytes.len() as u64,
            manifest_key_id: "test-key-1".into(),
        }
    }

    fn pass() -> Box<PostProbe<'static>> {
        Box::new(|_| Ok(true))
    }
    fn fail() -> Box<PostProbe<'static>> {
        Box::new(|_| Ok(false))
    }

    /// An approving fence takes the real floors lock, journals
    /// PostProbePassed under it, and the install commits — pinning both the
    /// fence contract and that a dropped fence argument cannot pass review
    /// silently (the veto twin below fails without one).
    #[test]
    fn fence_approval_commits_with_the_floors_lock_held() {
        let (_g, d) = dir();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let fence = |dd: &InstallDir| -> Result<Option<super::super::lock::UpgradeLock>, TxnError> {
            let guard = dd
                .try_lock_floors()
                .expect("floors lock probe")
                .expect("floors lock free in a fresh dir");
            Ok(Some(guard))
        };
        let out = run_install(
            &d,
            OldTarget::Absent,
            "1.0.0",
            &hash(b"NEW"),
            marker_for("1.0.0", b"NEW"),
            UpgradeState::default(),
            &pass(),
            Some(&fence),
        )
        .expect("test fixture operation should succeed");
        assert_eq!(out, TxnOutcome::Installed);
    }

    /// A vetoing fence rolls the upgrade back exactly like a failed probe:
    /// the previous target is restored byte-for-byte.
    #[test]
    fn fence_veto_rolls_back_and_restores_the_old_target() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let veto = |_: &InstallDir| -> Result<Option<super::super::lock::UpgradeLock>, TxnError> {
            Ok(None)
        };
        let out = run_install(
            &d,
            OldTarget::Present {
                hash: hash(b"OLD"),
                marker_snapshot: None,
            },
            "2.0.0",
            &hash(b"NEW"),
            marker_for("2.0.0", b"NEW"),
            UpgradeState::default(),
            &pass(),
            Some(&veto),
        )
        .expect("test fixture operation should succeed");
        assert_eq!(out, TxnOutcome::RolledBack);
        let restored = d
            .read_file(TARGET_BINARY_NAME)
            .expect("test fixture operation should succeed")
            .expect("target present");
        assert_eq!(restored, b"OLD");
    }

    /// With no custom fence, the DEFAULT policy check still vetoes a
    /// transaction superseded by a higher persisted control floor — this is
    /// the crash-recovery guarantee for the CandidateInstalled window.
    #[test]
    fn default_fence_vetoes_a_superseded_transaction() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        // A concurrent control decision already persisted revision 9.
        let newer = UpgradeState {
            max_control_revision: 9,
            ..UpgradeState::default()
        };
        write_state(&d, &newer).expect("test fixture operation should succeed");
        let txn_state = UpgradeState {
            max_control_revision: 7,
            ..UpgradeState::default()
        };
        let out = run_install(
            &d,
            OldTarget::Present {
                hash: hash(b"OLD"),
                marker_snapshot: None,
            },
            "2.0.0",
            &hash(b"NEW"),
            marker_for("2.0.0", b"NEW"),
            txn_state,
            &pass(),
            None,
        )
        .expect("test fixture operation should succeed");
        assert_eq!(out, TxnOutcome::RolledBack);
        let restored = d
            .read_file(TARGET_BINARY_NAME)
            .expect("test fixture operation should succeed")
            .expect("target present");
        assert_eq!(restored, b"OLD");
    }

    #[test]
    fn fresh_install_commits() {
        let (_g, d) = dir();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let out = run_install(
            &d,
            OldTarget::Absent,
            "1.0.0",
            &hash(b"NEW"),
            marker_for("1.0.0", b"NEW"),
            UpgradeState::default(),
            &pass(),
            None,
        )
        .expect("test fixture operation should succeed");
        assert_eq!(out, TxnOutcome::Installed);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"NEW"[..])
        );
        assert!(
            d.read_file(TXN_FILE_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
        assert!(
            d.read_file(CANDIDATE_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
        assert!(
            official_marker_for_target(&d, "darwin-arm64")
                .expect("test fixture operation should succeed")
                .is_some()
        );
    }

    #[test]
    fn fresh_install_probe_failure_aborts_and_leaves_nothing() {
        let (_g, d) = dir();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let out = run_install(
            &d,
            OldTarget::Absent,
            "1.0.0",
            &hash(b"NEW"),
            marker_for("1.0.0", b"NEW"),
            UpgradeState::default(),
            &fail(),
            None,
        )
        .expect("test fixture operation should succeed");
        assert_eq!(out, TxnOutcome::AbortedAbsent);
        assert!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
        assert!(
            d.read_file(TXN_FILE_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
    }

    #[test]
    fn upgrade_commits_and_replaces_target() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let out = run_install(
            &d,
            OldTarget::Present {
                hash: hash(b"OLD"),
                marker_snapshot: None,
            },
            "2.0.0",
            &hash(b"NEW"),
            marker_for("2.0.0", b"NEW"),
            UpgradeState::default(),
            &pass(),
            None,
        )
        .expect("test fixture operation should succeed");
        assert_eq!(out, TxnOutcome::Installed);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"NEW"[..])
        );
        assert!(
            d.read_file(BACKUP_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
    }

    #[test]
    fn upgrade_probe_failure_rolls_back_to_old_and_restores_marker() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let old_marker = marker_for("1.0.0", b"OLD");
        write_marker(&d, &old_marker).expect("test fixture operation should succeed");
        let out = run_install(
            &d,
            OldTarget::Present {
                hash: hash(b"OLD"),
                marker_snapshot: Some(old_marker),
            },
            "2.0.0",
            &hash(b"NEW"),
            marker_for("2.0.0", b"NEW"),
            UpgradeState::default(),
            &fail(),
            None,
        )
        .expect("test fixture operation should succeed");
        assert_eq!(out, TxnOutcome::RolledBack);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"OLD"[..])
        );
        // The restored marker still validates against the OLD binary.
        let m = official_marker_for_target(&d, "darwin-arm64")
            .expect("test fixture operation should succeed")
            .expect("test fixture operation should succeed");
        assert_eq!(m.version, "1.0.0");
        assert!(
            d.read_file(TXN_FILE_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
    }

    // ── §A.7 recovery decision table: construct each intermediate layout and
    //    assert the classified action drives to the right terminal state. ──

    fn journal(d: &InstallDir, txn: &Txn) {
        store_txn(d, txn).expect("test fixture operation should succeed");
    }

    fn base_txn(state: TxnState, old: OldTarget) -> Txn {
        Txn {
            schema_version: 1,
            state,
            old_target: old,
            new_version: "2.0.0".into(),
            new_hash: hash(b"NEW"),
            marker: marker_for("2.0.0", b"NEW"),
            new_state: UpgradeState::default(),
        }
    }

    #[test]
    fn rollback_and_abort_preserve_accepted_floors() {
        use crate::internal::upgrade::state::{STATE_FILE_NAME, read_state};

        // Post-probe failure → rollback: the manifest's verified floors must
        // survive even though the install is undone.
        let (_g, d) = dir();
        write_state(
            &d,
            &UpgradeState {
                generation_floor: 1,
                ..UpgradeState::default()
            },
        )
        .expect("test fixture operation should succeed");
        d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        let mut txn = base_txn(
            TxnState::CandidateInstalled,
            OldTarget::Present {
                hash: hash(b"OLD"),
                marker_snapshot: None,
            },
        );
        txn.new_state.generation_floor = 3;
        txn.new_state.max_control_revision = 9;
        txn.new_state.control_envelope_digest = Some("digest-9".into());
        journal(&d, &txn);
        assert_eq!(
            recover(&d, &fail()).expect("test fixture operation should succeed"),
            TxnOutcome::RolledBack
        );
        let state = read_state(&d).expect("test fixture operation should succeed");
        assert_eq!(state.generation_floor, 3);
        assert_eq!(state.max_control_revision, 9);
        assert_eq!(state.control_envelope_digest.as_deref(), Some("digest-9"));

        // Fresh-install abort: same preservation.
        let (_g2, d2) = dir();
        d2.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let mut txn2 = base_txn(TxnState::CandidateInstalled, OldTarget::Absent);
        txn2.new_state.generation_floor = 2;
        journal(&d2, &txn2);
        assert_eq!(
            recover(&d2, &fail()).expect("test fixture operation should succeed"),
            TxnOutcome::AbortedAbsent
        );
        assert_eq!(
            read_state(&d2)
                .expect("test fixture operation should succeed")
                .generation_floor,
            2
        );

        // Prepared-stage cleanup (crash before backup/rename) must also
        // preserve the floors: Prepared/Absent with only a candidate…
        let (_gp, dp) = dir();
        dp.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let mut txn_p = base_txn(TxnState::Prepared, OldTarget::Absent);
        txn_p.new_state.generation_floor = 5;
        journal(&dp, &txn_p);
        assert_eq!(
            recover(&dp, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::AbortedAbsent
        );
        assert_eq!(
            read_state(&dp)
                .expect("test fixture operation should succeed")
                .generation_floor,
            5
        );

        // …and Prepared/Present with target+candidate but no backup.
        let (_gq, dq) = dir();
        dq.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        dq.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        let mut txn_q = base_txn(
            TxnState::Prepared,
            OldTarget::Present {
                hash: hash(b"OLD"),
                marker_snapshot: None,
            },
        );
        txn_q.new_state.generation_floor = 6;
        journal(&dq, &txn_q);
        assert_eq!(
            recover(&dq, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::NoOp
        );
        assert_eq!(
            read_state(&dq)
                .expect("test fixture operation should succeed")
                .generation_floor,
            6
        );

        // Corrupt state must not drop the accepted floors: it is rebuilt from
        // the transaction's validated snapshot.
        let (_g3, d3) = dir();
        d3.write_file_atomic(STATE_FILE_NAME, b"{corrupt", 0o600)
            .expect("test fixture operation should succeed");
        d3.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        d3.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        let mut txn3 = base_txn(
            TxnState::CandidateInstalled,
            OldTarget::Present {
                hash: hash(b"OLD"),
                marker_snapshot: None,
            },
        );
        txn3.new_state.generation_floor = 4;
        journal(&d3, &txn3);
        assert_eq!(
            recover(&d3, &fail()).expect("test fixture operation should succeed"),
            TxnOutcome::RolledBack
        );
        assert_eq!(
            read_state(&d3)
                .expect("test fixture operation should succeed")
                .generation_floor,
            4
        );
    }

    #[test]
    fn recover_prepared_absent_candidate_only_aborts() {
        let (_g, d) = dir();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        journal(&d, &base_txn(TxnState::Prepared, OldTarget::Absent));
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::AbortedAbsent
        );
        assert!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
        assert!(
            d.read_file(TXN_FILE_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
    }

    #[test]
    fn recover_prepared_absent_rename_done_reprobes_and_commits() {
        let (_g, d) = dir();
        // rename landed (target=new, candidate gone) but state stayed Prepared.
        d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        journal(&d, &base_txn(TxnState::Prepared, OldTarget::Absent));
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::Installed
        );
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"NEW"[..])
        );
    }

    #[test]
    fn recover_prepared_present_no_backup_keeps_old() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d,
            &base_txn(
                TxnState::Prepared,
                OldTarget::Present {
                    hash: hash(b"OLD"),
                    marker_snapshot: None,
                },
            ),
        );
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::NoOp
        );
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"OLD"[..])
        );
        assert!(
            d.read_file(TXN_FILE_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
    }

    #[test]
    fn recover_prepared_present_with_backup_continues_overwrite() {
        let (_g, d) = dir();
        // Backup already made (target=old, backup=old, candidate=new).
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d,
            &base_txn(
                TxnState::Prepared,
                OldTarget::Present {
                    hash: hash(b"OLD"),
                    marker_snapshot: None,
                },
            ),
        );
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::Installed
        );
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"NEW"[..])
        );
    }

    #[test]
    fn recover_backup_durable_before_and_after_overwrite() {
        // (a) target still old.
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d,
            &base_txn(
                TxnState::BackupDurable,
                OldTarget::Present {
                    hash: hash(b"OLD"),
                    marker_snapshot: None,
                },
            ),
        );
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::Installed
        );

        // (b) rename already applied (target=new, candidate gone).
        let (_g2, d2) = dir();
        d2.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        d2.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d2,
            &base_txn(
                TxnState::BackupDurable,
                OldTarget::Present {
                    hash: hash(b"OLD"),
                    marker_snapshot: None,
                },
            ),
        );
        assert_eq!(
            recover(&d2, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::Installed
        );
    }

    #[test]
    fn recover_candidate_installed_reprobes_pass_and_fail() {
        // Pass → commit.
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d,
            &base_txn(
                TxnState::CandidateInstalled,
                OldTarget::Present {
                    hash: hash(b"OLD"),
                    marker_snapshot: None,
                },
            ),
        );
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::Installed
        );

        // Fail → rollback to old.
        let (_g2, d2) = dir();
        d2.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        d2.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d2,
            &base_txn(
                TxnState::CandidateInstalled,
                OldTarget::Present {
                    hash: hash(b"OLD"),
                    marker_snapshot: None,
                },
            ),
        );
        assert_eq!(
            recover(&d2, &fail()).expect("test fixture operation should succeed"),
            TxnOutcome::RolledBack
        );
        assert_eq!(
            d2.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"OLD"[..])
        );
    }

    #[test]
    fn recover_post_probe_passed_and_committed_are_idempotent() {
        for state in [TxnState::PostProbePassed, TxnState::Committed] {
            let (_g, d) = dir();
            d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
                .expect("test fixture operation should succeed");
            journal(&d, &base_txn(state, OldTarget::Absent));
            assert_eq!(
                recover(&d, &fail()).expect("test fixture operation should succeed"),
                TxnOutcome::Installed
            );
            assert!(
                d.read_file(TXN_FILE_NAME)
                    .expect("test fixture operation should succeed")
                    .is_none()
            );
            // Re-running recovery on the cleaned dir is a no-op.
            assert_eq!(
                recover(&d, &fail()).expect("test fixture operation should succeed"),
                TxnOutcome::NoOp
            );
        }
    }

    #[test]
    fn recover_rollback_and_abort_intents_complete() {
        // RollbackIntent, target still new + backup=old.
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d,
            &base_txn(
                TxnState::RollbackIntent,
                OldTarget::Present {
                    hash: hash(b"OLD"),
                    marker_snapshot: None,
                },
            ),
        );
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::RolledBack
        );
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .as_deref(),
            Some(&b"OLD"[..])
        );

        // AbortAbsentIntent, target=new leftover.
        let (_g2, d2) = dir();
        d2.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .expect("test fixture operation should succeed");
        journal(
            &d2,
            &base_txn(TxnState::AbortAbsentIntent, OldTarget::Absent),
        );
        assert_eq!(
            recover(&d2, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::AbortedAbsent
        );
        assert!(
            d2.read_file(TARGET_BINARY_NAME)
                .expect("test fixture operation should succeed")
                .is_none()
        );
    }

    #[test]
    fn recover_inconsistent_layout_is_fatal() {
        let (_g, d) = dir();
        // PostProbePassed but the target is missing entirely.
        journal(&d, &base_txn(TxnState::PostProbePassed, OldTarget::Absent));
        assert!(matches!(
            recover(&d, &pass()),
            Err(TxnError::FatalRecovery { .. })
        ));
    }

    #[test]
    fn recover_without_txn_is_noop() {
        let (_g, d) = dir();
        assert_eq!(
            recover(&d, &pass()).expect("test fixture operation should succeed"),
            TxnOutcome::NoOp
        );
    }
}
