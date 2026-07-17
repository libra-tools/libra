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
    state::{UpgradeState, write_state},
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
    write_state(dir.path(), &txn.new_state).map_err(|e| TxnError::State(e.to_string()))?;
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
fn probe_and_resolve(
    dir: &InstallDir,
    txn: &mut Txn,
    post_probe: &PostProbe<'_>,
) -> Result<TxnOutcome, TxnError> {
    if post_probe(dir)? {
        txn.state = TxnState::PostProbePassed;
        store_txn(dir, txn)?;
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
                dir.remove_file(CANDIDATE_NAME)?;
                dir.fsync_dir()?;
                dir.remove_file(TXN_FILE_NAME)?;
                dir.fsync_dir()?;
                Ok(TxnOutcome::AbortedAbsent)
            } else if layout.target.as_deref() == Some(new.as_str()) && layout.candidate.is_none() {
                // rename landed but state was not yet advanced.
                txn.state = TxnState::CandidateInstalled;
                store_txn(dir, &txn)?;
                probe_and_resolve(dir, &mut txn, post_probe)
            } else {
                Err(fatal("Prepared/Absent layout unrecognized"))
            }
        }
        (TxnState::Prepared, OldTarget::Present { hash, .. }) => {
            let target_old = layout.target.as_deref() == Some(hash.as_str());
            let candidate_new = layout.candidate.as_deref() == Some(new.as_str());
            if target_old && candidate_new && layout.backup.is_none() {
                dir.remove_file(CANDIDATE_NAME)?;
                dir.fsync_dir()?;
                dir.remove_file(TXN_FILE_NAME)?;
                dir.fsync_dir()?;
                Ok(TxnOutcome::NoOp)
            } else if target_old && candidate_new && layout.backup.as_deref() == Some(hash.as_str())
            {
                txn.state = TxnState::BackupDurable;
                store_txn(dir, &txn)?;
                continue_overwrite_from_backup_durable(dir, &mut txn, post_probe)
            } else {
                Err(fatal("Prepared/Present layout unrecognized"))
            }
        }
        (TxnState::BackupDurable, OldTarget::Present { hash, .. }) => {
            let candidate_new = layout.candidate.as_deref() == Some(new.as_str());
            let backup_old = layout.backup.as_deref() == Some(hash.as_str());
            if layout.target.as_deref() == Some(hash.as_str()) && candidate_new && backup_old {
                continue_overwrite_from_backup_durable(dir, &mut txn, post_probe)
            } else if layout.target.as_deref() == Some(new.as_str())
                && layout.candidate.is_none()
                && backup_old
            {
                txn.state = TxnState::CandidateInstalled;
                store_txn(dir, &txn)?;
                probe_and_resolve(dir, &mut txn, post_probe)
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
            probe_and_resolve(dir, &mut txn, post_probe)
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
) -> Result<TxnOutcome, TxnError> {
    dir.rename_entry(CANDIDATE_NAME, TARGET_BINARY_NAME)?;
    dir.fsync_dir()?;
    txn.state = TxnState::CandidateInstalled;
    store_txn(dir, txn)?;
    probe_and_resolve(dir, txn, post_probe)
}

/// Drive a fresh transaction to completion. The caller has already written
/// the verified candidate to [`CANDIDATE_NAME`] (via [`InstallDir`]) and
/// holds the §A.5 lock. `old_target` reflects the pre-install target.
pub fn run_install(
    dir: &InstallDir,
    old_target: OldTarget,
    new_version: &str,
    new_hash: &str,
    marker: InstallMarker,
    new_state: UpgradeState,
    post_probe: &PostProbe<'_>,
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
            probe_and_resolve(dir, &mut txn, post_probe)
        }
        OldTarget::Present { .. } => {
            // Upgrade: durable backup BEFORE overwrite.
            dir.rename_entry(TARGET_BINARY_NAME, BACKUP_NAME)?;
            dir.fsync_dir()?;
            txn.state = TxnState::BackupDurable;
            store_txn(dir, &txn)?;
            continue_overwrite_from_backup_durable(dir, &mut txn, post_probe)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::internal::upgrade::marker::{OFFICIAL_INSTALL_SOURCE, official_marker_for_target};

    fn dir() -> (tempfile::TempDir, InstallDir) {
        let guard = tempfile::tempdir().unwrap();
        let path = guard.path().canonicalize().unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let d = InstallDir::open_validated(&path).unwrap();
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

    #[test]
    fn fresh_install_commits() {
        let (_g, d) = dir();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
        let out = run_install(
            &d,
            OldTarget::Absent,
            "1.0.0",
            &hash(b"NEW"),
            marker_for("1.0.0", b"NEW"),
            UpgradeState::default(),
            &pass(),
        )
        .unwrap();
        assert_eq!(out, TxnOutcome::Installed);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"NEW"[..])
        );
        assert!(d.read_file(TXN_FILE_NAME).unwrap().is_none());
        assert!(d.read_file(CANDIDATE_NAME).unwrap().is_none());
        assert!(
            official_marker_for_target(&d, "darwin-arm64")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn fresh_install_probe_failure_aborts_and_leaves_nothing() {
        let (_g, d) = dir();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
        let out = run_install(
            &d,
            OldTarget::Absent,
            "1.0.0",
            &hash(b"NEW"),
            marker_for("1.0.0", b"NEW"),
            UpgradeState::default(),
            &fail(),
        )
        .unwrap();
        assert_eq!(out, TxnOutcome::AbortedAbsent);
        assert!(d.read_file(TARGET_BINARY_NAME).unwrap().is_none());
        assert!(d.read_file(TXN_FILE_NAME).unwrap().is_none());
    }

    #[test]
    fn upgrade_commits_and_replaces_target() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .unwrap();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
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
        )
        .unwrap();
        assert_eq!(out, TxnOutcome::Installed);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"NEW"[..])
        );
        assert!(d.read_file(BACKUP_NAME).unwrap().is_none());
    }

    #[test]
    fn upgrade_probe_failure_rolls_back_to_old_and_restores_marker() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .unwrap();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
        let old_marker = marker_for("1.0.0", b"OLD");
        write_marker(&d, &old_marker).unwrap();
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
        )
        .unwrap();
        assert_eq!(out, TxnOutcome::RolledBack);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"OLD"[..])
        );
        // The restored marker still validates against the OLD binary.
        let m = official_marker_for_target(&d, "darwin-arm64")
            .unwrap()
            .unwrap();
        assert_eq!(m.version, "1.0.0");
        assert!(d.read_file(TXN_FILE_NAME).unwrap().is_none());
    }

    // ── §A.7 recovery decision table: construct each intermediate layout and
    //    assert the classified action drives to the right terminal state. ──

    fn journal(d: &InstallDir, txn: &Txn) {
        store_txn(d, txn).unwrap();
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
    fn recover_prepared_absent_candidate_only_aborts() {
        let (_g, d) = dir();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
        journal(&d, &base_txn(TxnState::Prepared, OldTarget::Absent));
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::AbortedAbsent);
        assert!(d.read_file(TARGET_BINARY_NAME).unwrap().is_none());
        assert!(d.read_file(TXN_FILE_NAME).unwrap().is_none());
    }

    #[test]
    fn recover_prepared_absent_rename_done_reprobes_and_commits() {
        let (_g, d) = dir();
        // rename landed (target=new, candidate gone) but state stayed Prepared.
        d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .unwrap();
        journal(&d, &base_txn(TxnState::Prepared, OldTarget::Absent));
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::Installed);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"NEW"[..])
        );
    }

    #[test]
    fn recover_prepared_present_no_backup_keeps_old() {
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .unwrap();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
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
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::NoOp);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"OLD"[..])
        );
        assert!(d.read_file(TXN_FILE_NAME).unwrap().is_none());
    }

    #[test]
    fn recover_prepared_present_with_backup_continues_overwrite() {
        let (_g, d) = dir();
        // Backup already made (target=old, backup=old, candidate=new).
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .unwrap();
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755).unwrap();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
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
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::Installed);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"NEW"[..])
        );
    }

    #[test]
    fn recover_backup_durable_before_and_after_overwrite() {
        // (a) target still old.
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"OLD", 0o755)
            .unwrap();
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755).unwrap();
        d.write_file_atomic(CANDIDATE_NAME, b"NEW", 0o755).unwrap();
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
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::Installed);

        // (b) rename already applied (target=new, candidate gone).
        let (_g2, d2) = dir();
        d2.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .unwrap();
        d2.write_file_atomic(BACKUP_NAME, b"OLD", 0o755).unwrap();
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
        assert_eq!(recover(&d2, &pass()).unwrap(), TxnOutcome::Installed);
    }

    #[test]
    fn recover_candidate_installed_reprobes_pass_and_fail() {
        // Pass → commit.
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .unwrap();
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755).unwrap();
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
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::Installed);

        // Fail → rollback to old.
        let (_g2, d2) = dir();
        d2.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .unwrap();
        d2.write_file_atomic(BACKUP_NAME, b"OLD", 0o755).unwrap();
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
        assert_eq!(recover(&d2, &fail()).unwrap(), TxnOutcome::RolledBack);
        assert_eq!(
            d2.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"OLD"[..])
        );
    }

    #[test]
    fn recover_post_probe_passed_and_committed_are_idempotent() {
        for state in [TxnState::PostProbePassed, TxnState::Committed] {
            let (_g, d) = dir();
            d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
                .unwrap();
            journal(&d, &base_txn(state, OldTarget::Absent));
            assert_eq!(recover(&d, &fail()).unwrap(), TxnOutcome::Installed);
            assert!(d.read_file(TXN_FILE_NAME).unwrap().is_none());
            // Re-running recovery on the cleaned dir is a no-op.
            assert_eq!(recover(&d, &fail()).unwrap(), TxnOutcome::NoOp);
        }
    }

    #[test]
    fn recover_rollback_and_abort_intents_complete() {
        // RollbackIntent, target still new + backup=old.
        let (_g, d) = dir();
        d.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .unwrap();
        d.write_file_atomic(BACKUP_NAME, b"OLD", 0o755).unwrap();
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
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::RolledBack);
        assert_eq!(
            d.read_file(TARGET_BINARY_NAME).unwrap().as_deref(),
            Some(&b"OLD"[..])
        );

        // AbortAbsentIntent, target=new leftover.
        let (_g2, d2) = dir();
        d2.write_file_atomic(TARGET_BINARY_NAME, b"NEW", 0o755)
            .unwrap();
        journal(
            &d2,
            &base_txn(TxnState::AbortAbsentIntent, OldTarget::Absent),
        );
        assert_eq!(recover(&d2, &pass()).unwrap(), TxnOutcome::AbortedAbsent);
        assert!(d2.read_file(TARGET_BINARY_NAME).unwrap().is_none());
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
        assert_eq!(recover(&d, &pass()).unwrap(), TxnOutcome::NoOp);
    }
}
