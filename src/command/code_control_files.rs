//! Local TUI automation control file lifecycle.
//!
//! `libra code --control write` uses three files under the ACTING worktree's
//! local gitdir by default (plan-20260714 §C.8 W4 — for the main worktree the
//! local gitdir IS the repository storage root, so the historical paths are
//! unchanged there):
//!
//! - `.libra/code/control-token` stores the per-process bearer token.
//! - `.libra/code/control.json` stores non-secret endpoint discovery metadata.
//! - `.libra/code/control.lock` is an advisory single-instance lock.
//!
//! The lock is the owner contract: callers must acquire it before writing a new
//! token, so a second write-enabled instance cannot silently replace the first
//! instance's credentials. Stale `control.json` files from crashed processes are
//! ignored when their PID is not live. On Unix the token file must be a regular
//! non-symlink file with exact `0600` permissions; Windows currently treats the
//! permission check as a no-op because ACL semantics need a separate design.
//!
//! ## Scope contract (plan-20260714 §C.8 W4)
//!
//! A version-2 [`ControlInfo`] carries the WRITER's scope — `repo_id`,
//! `worktree_id`, and (when the writer holds a workspace lease) `workspace_id`
//! plus `lease_fence`. Every consumer that trusts, replaces, or removes an
//! existing control file must first classify it against its OWN scope via
//! [`classify_control_scope`]:
//!
//! - a scope mismatch is refused even when the recorded PID is dead — stale
//!   cleanup must never delete another worktree's/workspace's control files or
//!   release a newer owner;
//! - a legacy (pre-v2) file with no scope fields is adoptable only while the
//!   repository has no linked-worktree evidence; with such evidence it is
//!   AMBIGUOUS and never auto-adopted (remove it manually after confirming the
//!   owning process is gone). ONE exception keeps upgrades survivable: a
//!   [`ControlScopePolicy::Repository`] surface adopts a legacy record whose
//!   `working_dir` still resolves to this repository's storage — that proves
//!   the repository, which is the only thing the repository policy asks
//!   (see `legacy_record_names_this_repository`).

#[cfg(unix)]
use std::os::unix::{fs::OpenOptionsExt, fs::PermissionsExt, io::AsRawFd};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use super::code::resolve_storage_root;
use crate::{
    internal::{db, workspace::RepoIdentity},
    utils::util,
};

/// The control-info schema version this binary writes. Version 2 added the
/// writer-scope fields (`repoId`/`worktreeId`/`workspaceId`/`leaseFence`);
/// files at version 1 (or missing a `repoId`) follow the legacy-ambiguity
/// rules in [`classify_control_scope`].
pub const CONTROL_INFO_VERSION: u8 = 2;

/// Discovery metadata written to `control.json`.
///
/// This struct intentionally contains no control token, token hash, token path,
/// provider credentials, request body, or environment dump.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlInfo {
    pub version: u8,
    pub mode: String,
    pub pid: u32,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_url: Option<String>,
    pub working_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub started_at: DateTime<Utc>,
    /// Stable repository identity (`libra.repoid`) of the writer. Always
    /// `Some` in version-2 files; `None` marks a legacy file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    /// The writer's worktree scope: `None` = the main worktree (authoritative
    /// in version-2 files; indistinguishable from "unknown" in legacy files —
    /// the `version`/`repo_id` pair discriminates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    /// Workspace lease association, present only when the writer holds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_fence: Option<i64>,
}

impl ControlInfo {
    /// True when this file carries an authoritative writer scope.
    pub fn has_scope(&self) -> bool {
        self.version >= 2 && self.repo_id.is_some()
    }
}

/// The scope a control-file writer or consumer is acting from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlScope {
    /// Stable repository identity (`libra.repoid`).
    pub repo_id: String,
    /// `None` = the main worktree.
    pub worktree_id: Option<String>,
    /// Workspace lease held by this process, when any.
    pub workspace_id: Option<String>,
    pub lease_fence: Option<i64>,
}

/// How strictly a sidecar is bound to one worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlScopePolicy {
    /// Per-worktree sidecar (`libra code`): repository AND worktree must
    /// match; workspace/fence must match when both sides carry one.
    Worktree,
    /// Repository-level sidecar (`libra service`): only the repository must
    /// match — any worktree of the same repository may reclaim a stale
    /// instance (the advisory lock still arbitrates liveness).
    Repository,
}

/// Field-level mismatch between an existing control file and the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlScopeMismatch {
    RepoId {
        found: String,
        expected: String,
    },
    WorktreeId {
        found: Option<String>,
        expected: Option<String>,
    },
    WorkspaceId {
        found: String,
        expected: String,
    },
    LeaseFence {
        found: i64,
        expected: i64,
    },
}

impl fmt::Display for ControlScopeMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let describe = |id: &Option<String>| match id {
            Some(id) => format!("linked worktree '{id}'"),
            None => "the main worktree".to_string(),
        };
        match self {
            Self::RepoId { found, expected } => write!(
                f,
                "it belongs to repository '{found}' (this repository is '{expected}')"
            ),
            Self::WorktreeId { found, expected } => write!(
                f,
                "it belongs to {} (this scope is {})",
                describe(found),
                describe(expected)
            ),
            Self::WorkspaceId { found, expected } => write!(
                f,
                "it belongs to workspace '{found}' (this scope is workspace '{expected}')"
            ),
            Self::LeaseFence { found, expected } => write!(
                f,
                "it was written at lease fence {found} (this scope holds fence {expected})"
            ),
        }
    }
}

/// Classification of an existing control file against the caller's scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlScopeCheck {
    /// Version-2 file written from this same scope.
    Match,
    /// Legacy file and the repository has no linked-worktree evidence: treat
    /// as this repository's main scope (pre-W4 behavior).
    LegacyAdoptable,
    /// Legacy file but linked worktrees exist(ed): the owner scope cannot be
    /// attributed — never auto-adopt, never auto-delete.
    LegacyAmbiguous,
    /// Version-2 file written from a DIFFERENT scope: refuse to trust,
    /// replace, or remove it, even when its PID is dead.
    Foreign(ControlScopeMismatch),
}

/// Classify an existing control-info file against the caller's scope
/// (plan-20260714 §C.8 W4). Pure — filesystem/PID liveness is deliberately
/// out of scope so the matrix stays unit-testable.
pub fn classify_control_scope(
    info: &ControlInfo,
    expected: &ControlScope,
    policy: ControlScopePolicy,
    linked_evidence: bool,
) -> ControlScopeCheck {
    let Some(found_repo) = info.repo_id.as_deref().filter(|_| info.version >= 2) else {
        return if linked_evidence {
            ControlScopeCheck::LegacyAmbiguous
        } else {
            ControlScopeCheck::LegacyAdoptable
        };
    };

    if found_repo != expected.repo_id {
        return ControlScopeCheck::Foreign(ControlScopeMismatch::RepoId {
            found: found_repo.to_string(),
            expected: expected.repo_id.clone(),
        });
    }
    if policy == ControlScopePolicy::Worktree && info.worktree_id != expected.worktree_id {
        return ControlScopeCheck::Foreign(ControlScopeMismatch::WorktreeId {
            found: info.worktree_id.clone(),
            expected: expected.worktree_id.clone(),
        });
    }
    if let (Some(found), Some(expected_ws)) = (
        info.workspace_id.as_deref(),
        expected.workspace_id.as_deref(),
    ) && found != expected_ws
    {
        return ControlScopeCheck::Foreign(ControlScopeMismatch::WorkspaceId {
            found: found.to_string(),
            expected: expected_ws.to_string(),
        });
    }
    if let (Some(found), Some(expected_fence)) = (info.lease_fence, expected.lease_fence)
        && found != expected_fence
    {
        return ControlScopeCheck::Foreign(ControlScopeMismatch::LeaseFence {
            found,
            expected: expected_fence,
        });
    }
    ControlScopeCheck::Match
}

/// Whether the repository at `storage_path` has linked-worktree evidence
/// (§C.8 W4 legacy-ambiguity input). An unreadable or corrupt registry counts
/// as evidence — never guess "single worktree" from a registry that cannot
/// prove it.
pub fn repo_has_linked_evidence(storage_path: &Path) -> bool {
    let registry = storage_path.join("worktrees.json");
    match fs::read_to_string(&registry) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Ok(raw) => !crate::command::worktree::WorktreeState::parse(raw.as_bytes())
            .is_ok_and(|state| state.is_single_main()),
        Err(_) => true,
    }
}

/// Resolve the caller's [`ControlScope`] for `working_dir`: the repository
/// identity comes from `libra.repoid` (minted lazily for legacy repositories,
/// same fact source as the workspace store), the worktree id from the
/// workdir's local gitdir. `workspace` carries a held lease, when any.
pub async fn resolve_control_scope(
    working_dir: &Path,
    workspace: Option<(String, i64)>,
) -> Result<ControlScope> {
    let storage_path =
        util::try_get_storage_path(Some(working_dir.to_path_buf())).with_context(|| {
            format!(
                "cannot resolve the repository storage root for '{}'",
                working_dir.display()
            )
        })?;
    let conn = db::get_db_conn_instance_for_path(&storage_path.join(util::DATABASE))
        .await
        .with_context(|| {
            format!(
                "cannot open the repository database under '{}'",
                storage_path.display()
            )
        })?;
    let repo_id = RepoIdentity::resolve_or_init(&conn)
        .await
        .context("cannot resolve the repository identity for the control scope")?;
    let (workspace_id, lease_fence) = match workspace {
        Some((id, fence)) => (Some(id), Some(fence)),
        None => (None, None),
    };
    Ok(ControlScope {
        repo_id: repo_id.as_str().to_string(),
        worktree_id: util::worktree_id_for_base(Some(working_dir.to_path_buf())),
        workspace_id,
        lease_fence,
    })
}

/// Scope refusals raised before trusting/replacing/removing a control file.
#[derive(Debug)]
pub enum ControlScopeError {
    Foreign {
        path: PathBuf,
        mismatch: ControlScopeMismatch,
    },
    LegacyAmbiguous {
        path: PathBuf,
    },
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for ControlScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Foreign { path, mismatch } => write!(
                f,
                "CONTROL_SCOPE_CONFLICT: control file '{}' was not written from this scope: \
                 {mismatch}. Refusing to reuse or remove another scope's control sidecar — run \
                 from the owning worktree, or remove the file manually after confirming its \
                 process is gone.",
                path.display()
            ),
            Self::LegacyAmbiguous { path } => write!(
                f,
                "CONTROL_SCOPE_CONFLICT: control file '{}' predates scope stamping and this \
                 repository has linked worktrees, so its owner cannot be attributed. Remove the \
                 file manually after confirming the owning process is gone.",
                path.display()
            ),
            Self::Unreadable { path, source } => write!(
                f,
                "cannot read control file '{}' to verify its scope: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ControlScopeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Gate a takeover/overwrite of `info_path` on scope compatibility
/// (§C.8 W4): a missing file is fine, a same-scope or adoptable-legacy file
/// may be replaced (the advisory lock arbitrates liveness), anything else is
/// refused — INCLUDING dead-PID files from a foreign scope, whose cleanup
/// belongs to their own scope's owner or an explicit human action.
/// Repository-policy adoption of a LEGACY (pre-scope-stamping) record.
///
/// A version-1 file cannot name its writer's repository — but it does name
/// the writer's `working_dir`. Under [`ControlScopePolicy::Repository`] the
/// only question is whether the record belongs to THIS repository (which
/// worktree wrote it is irrelevant by policy), so a `working_dir` that
/// still resolves to this repository's common storage settles ownership
/// even when linked worktrees exist.
///
/// This is what keeps an UPGRADE survivable: a service killed before the
/// scope stamping shipped leaves a v1 record, and in any repository that
/// has ever had a linked worktree that record would otherwise be
/// permanently un-takeoverable — the next `libra service run` would refuse
/// to start and demand a manual file deletion.
///
/// Absent proof — a moved or deleted working directory, a path in another
/// repository, an unresolvable resolve — the record stays ambiguous and the
/// gate fails closed.
fn legacy_record_names_this_repository(info: &ControlInfo, common_storage: &Path) -> bool {
    let Ok(record_storage) = util::try_get_storage_path(Some(info.working_dir.clone())) else {
        return false;
    };
    // Both sides go through the same canonicalization: a record written via
    // `/var/...` must still match a common storage discovered as
    // `/private/var/...` (macOS), and vice versa.
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(&record_storage) == canonical(common_storage)
}

pub fn ensure_scope_takeover_allowed(
    info_path: &Path,
    expected: &ControlScope,
    policy: ControlScopePolicy,
    linked_evidence: bool,
    common_storage: &Path,
) -> std::result::Result<(), ControlScopeError> {
    let content = match fs::read_to_string(info_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ControlScopeError::Unreadable {
                path: info_path.to_path_buf(),
                source,
            });
        }
        Ok(content) => content,
    };
    let info: ControlInfo = match serde_json::from_str(&content) {
        Ok(info) => info,
        Err(error) => {
            // A malformed file cannot prove its scope. Without linked
            // evidence keep the historical "stale garbage is replaceable"
            // behavior; with linked evidence fail closed like a legacy file.
            if linked_evidence {
                return Err(ControlScopeError::LegacyAmbiguous {
                    path: info_path.to_path_buf(),
                });
            }
            tracing::debug!(
                path = %info_path.display(),
                error = %error,
                "replacing malformed control info file"
            );
            return Ok(());
        }
    };
    match classify_control_scope(&info, expected, policy, linked_evidence) {
        ControlScopeCheck::Match | ControlScopeCheck::LegacyAdoptable => Ok(()),
        // A repository-level surface may still adopt a legacy record that
        // PROVES it belongs here (see the helper: this is the upgrade path).
        ControlScopeCheck::LegacyAmbiguous
            if policy == ControlScopePolicy::Repository
                && legacy_record_names_this_repository(&info, common_storage) =>
        {
            tracing::debug!(
                path = %info_path.display(),
                working_dir = %info.working_dir.display(),
                "adopting a legacy control record whose working dir resolves to this repository"
            );
            Ok(())
        }
        ControlScopeCheck::LegacyAmbiguous => Err(ControlScopeError::LegacyAmbiguous {
            path: info_path.to_path_buf(),
        }),
        ControlScopeCheck::Foreign(mismatch) => Err(ControlScopeError::Foreign {
            path: info_path.to_path_buf(),
            mismatch,
        }),
    }
}

/// Resolved token, info, and lock paths for a control-enabled session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlPaths {
    pub token: PathBuf,
    pub info: PathBuf,
    pub lock: PathBuf,
}

/// Best-effort summary of an existing live control instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveInstanceInfo {
    pub pid: u32,
    pub base_url: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
}

/// Advisory lock guard. Dropping releases the lock and best-effort removes the
/// lock file to keep manual inspection clear after normal shutdown.
#[derive(Debug)]
pub struct ControlLockGuard {
    file: File,
    lock_path: PathBuf,
}

impl Drop for ControlLockGuard {
    fn drop(&mut self) {
        let _ = unlock_file(&self.file);
        if let Err(error) = fs::remove_file(&self.lock_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(
                path = %self.lock_path.display(),
                error = %error,
                "failed to remove local TUI control lock file"
            );
        }
    }
}

/// Errors returned while acquiring the write-control single-instance lock.
#[derive(Debug)]
pub enum ControlLockError {
    AlreadyHeld {
        existing: Option<LiveInstanceInfo>,
        info_path: PathBuf,
        lock_path: PathBuf,
    },
    Io(std::io::Error),
}

impl fmt::Display for ControlLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyHeld {
                existing: Some(existing),
                info_path,
                lock_path,
            } => {
                write!(
                    f,
                    "CONTROL_INSTANCE_CONFLICT: another `libra code --control write` instance is active"
                )?;
                write!(f, " (pid: {}", existing.pid)?;
                if let Some(base_url) = &existing.base_url {
                    write!(f, ", baseUrl: {base_url}")?;
                }
                write!(
                    f,
                    "). info: {}, lock: {}. Stop the existing instance (Ctrl-C / kill {}) or pass `--control-token-file` and `--control-info-file` to use separate paths.",
                    info_path.display(),
                    lock_path.display(),
                    existing.pid
                )
            }
            Self::AlreadyHeld {
                existing: None,
                info_path,
                lock_path,
            } => write!(
                f,
                "CONTROL_INSTANCE_CONFLICT: another `libra code --control write` instance holds the control lock. info: {}, lock: {}. Stop the existing instance or pass `--control-token-file` and `--control-info-file` to use separate paths.",
                info_path.display(),
                lock_path.display()
            ),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ControlLockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::AlreadyHeld { .. } => None,
        }
    }
}

/// Resolve default or overridden local-control paths.
///
/// The default directory is the ACTING worktree's local gitdir `code/`
/// subdirectory (§C.8 W4): for the main worktree that is the historical
/// common `.libra/code/`, for a linked worktree its own private gitdir — so
/// two worktrees can never share a token/info/lock by default.
pub fn resolve_control_paths(
    working_dir: &Path,
    token_override: Option<&Path>,
    info_override: Option<&Path>,
) -> ControlPaths {
    let control_dir = match util::try_get_worktree_gitdir(Some(working_dir.to_path_buf())) {
        Ok(gitdir) => gitdir.join("code"),
        Err(error) => {
            // Same non-silent degradation contract as `resolve_storage_root`:
            // a broken linked worktree must be diagnosable, not phantom-routed.
            tracing::warn!(
                working_dir = %working_dir.display(),
                %error,
                "worktree gitdir resolution failed; control files fall back to the storage root — \
                 if this is a linked worktree, run `libra worktree repair --confirm <worktree-path>`"
            );
            // §C.4.1: with no gitdir AND no storage root there is nowhere
            // legitimate to put control files — use the working directory
            // explicitly rather than minting a `.libra` that looks like a
            // repository root to everything downstream.
            resolve_storage_root(working_dir)
                .unwrap_or_else(|| working_dir.to_path_buf())
                .join("code")
        }
    };
    let token = token_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| control_dir.join("control-token"));
    let info = info_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| control_dir.join("control.json"));
    let lock = info.with_extension("lock");
    ControlPaths { token, info, lock }
}

/// Acquire the write-control advisory lock, failing fast when another live
/// process already owns it.
pub fn acquire_control_lock(
    lock_path: &Path,
) -> std::result::Result<ControlLockGuard, ControlLockError> {
    if let Some(parent) = lock_path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return Err(ControlLockError::Io(error));
    }

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(ControlLockError::Io)?;

    match try_lock_file_exclusive(&file) {
        Ok(true) => {}
        Ok(false) => {
            let info_path = lock_path.with_extension("json");
            let existing = inspect_existing_instance(&info_path).ok().flatten();
            return Err(ControlLockError::AlreadyHeld {
                existing,
                info_path,
                lock_path: lock_path.to_path_buf(),
            });
        }
        Err(error) => return Err(ControlLockError::Io(error)),
    }

    if let Err(error) = write_lock_pid(&file) {
        let _ = unlock_file(&file);
        return Err(ControlLockError::Io(error));
    }

    Ok(ControlLockGuard {
        file,
        lock_path: lock_path.to_path_buf(),
    })
}

fn write_lock_pid(mut file: &File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

/// Return a live instance if `control.json` points at a process that still
/// exists. Malformed or stale files return `Ok(None)`.
pub fn inspect_existing_instance(info_path: &Path) -> Result<Option<LiveInstanceInfo>> {
    let content = match fs::read_to_string(info_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to read existing control info"),
    };

    let info: ControlInfo = match serde_json::from_str(&content) {
        Ok(info) => info,
        Err(error) => {
            tracing::debug!(
                path = %info_path.display(),
                error = %error,
                "ignoring malformed local TUI control info file"
            );
            return Ok(None);
        }
    };

    if !pid_is_live(info.pid) {
        return Ok(None);
    }

    Ok(Some(LiveInstanceInfo {
        pid: info.pid,
        base_url: Some(info.base_url),
        started_at: Some(info.started_at),
    }))
}

/// Create or overwrite the per-process control token file.
///
/// The caller must already hold [`ControlLockGuard`]; this function enforces
/// file type and permissions but deliberately does not perform a second
/// concurrency check.
pub async fn ensure_control_token_file(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create control token parent directory '{}'",
                parent.display()
            )
        })?;
    }

    let token = generate_control_token()?;

    if path.exists() || fs::symlink_metadata(path).is_ok() {
        validate_token_file_perms(path)?;
        let mut file = writable_token_file(path, false)?;
        file.set_len(0).with_context(|| {
            format!("failed to truncate control token file '{}'", path.display())
        })?;
        file.write_all(token.as_bytes())
            .with_context(|| format!("failed to write control token file '{}'", path.display()))?;
        file.flush()
            .with_context(|| format!("failed to flush control token file '{}'", path.display()))?;
        return Ok(token);
    }

    let mut file = writable_token_file(path, true)?;
    file.write_all(token.as_bytes())
        .with_context(|| format!("failed to write control token file '{}'", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush control token file '{}'", path.display()))?;
    Ok(token)
}

fn generate_control_token() -> Result<String> {
    let rng = SystemRandom::new();
    let mut token = [0u8; 32];
    rng.fill(&mut token)
        .map_err(|_| anyhow!("failed to generate secure local TUI control token"))?;
    Ok(URL_SAFE_NO_PAD.encode(token))
}

/// Validate that the token path is a regular `0600` file on Unix.
pub fn validate_token_file_perms(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect control token file '{}'", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "control token file '{}' must not be a symlink",
            path.display()
        );
    }
    if !metadata.file_type().is_file() {
        bail!(
            "control token path '{}' must be a regular file",
            path.display()
        );
    }
    validate_token_file_mode(path, &metadata)
}

#[cfg(unix)]
fn validate_token_file_mode(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        bail!(
            "control token file '{}' must have permissions 0600 (currently {:03o}); run: chmod 0600 {}",
            path.display(),
            mode,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_token_file_mode(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn writable_token_file(path: &Path, create_new: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    if create_new {
        options.create_new(true);
    } else {
        options.create(false).truncate(false);
    }
    options
        .open(path)
        .with_context(|| format!("failed to open control token file '{}'", path.display()))
}

#[cfg(not(unix))]
fn writable_token_file(path: &Path, create_new: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(false).truncate(false);
    }
    options
        .open(path)
        .with_context(|| format!("failed to open control token file '{}'", path.display()))
}

/// Write non-secret local-control discovery metadata.
pub fn write_control_info(path: &Path, info: &ControlInfo) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create control info parent directory '{}'",
                parent.display()
            )
        })?;
    }
    let serialized =
        serde_json::to_string_pretty(info).context("failed to serialize local TUI control info")?;
    fs::write(path, serialized)
        .with_context(|| format!("failed to write control info file '{}'", path.display()))
}

/// Best-effort cleanup for token/info files on normal shutdown or startup
/// failure. Lock file cleanup is owned by [`ControlLockGuard::drop`].
///
/// The info file is removed only when it still records THIS process's PID
/// (§C.8 W4): an observe-mode writer holds no lock, and after a crash-reclaim
/// race the file may already belong to a successor — shutdown cleanup must
/// never delete a control file it no longer owns.
pub fn cleanup_control_files(paths: &ControlPaths, remove_token: bool, remove_info: bool) {
    let cleanup_paths = [
        remove_token.then_some(&paths.token),
        remove_info.then_some(&paths.info),
    ];
    for path in cleanup_paths.into_iter().flatten() {
        if *path == paths.info && !info_file_is_owned_by_this_process(path) {
            tracing::debug!(
                path = %path.display(),
                "skipping control info cleanup: the file no longer records this process"
            );
            continue;
        }
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(
                path = %path.display(),
                error = %error,
                "failed to remove local TUI control file"
            );
        }
    }
}

/// True when the info file parses and records this process's PID. A missing
/// file counts as owned (the remove below is a no-op); a malformed or
/// foreign-PID file does not — leave it for its actual owner.
fn info_file_is_owned_by_this_process(path: &Path) -> bool {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
        Ok(content) => serde_json::from_str::<ControlInfo>(&content)
            .map(|info| info.pid == std::process::id())
            .unwrap_or(false),
    }
}

/// Return whether a PID appears to still be live.
pub fn pid_is_live(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    pid_is_live_impl(pid)
}

#[cfg(unix)]
fn pid_is_live_impl(pid: u32) -> bool {
    if pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::ESRCH => false,
        Some(code) if code == libc::EPERM => true,
        _ => true,
    }
}

#[cfg(not(unix))]
fn pid_is_live_impl(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn try_lock_file_exclusive(file: &File) -> std::io::Result<bool> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(false),
        _ => Err(error),
    }
}

#[cfg(unix)]
fn unlock_file(file: &File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn try_lock_file_exclusive(_file: &File) -> std::io::Result<bool> {
    Ok(true)
}

#[cfg(not(unix))]
fn unlock_file(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{fs::Permissions, os::unix::fs as unix_fs};

    use super::*;

    fn test_control_info(pid: u32, base_url: &str) -> ControlInfo {
        ControlInfo {
            version: 1,
            mode: "write".to_string(),
            pid,
            base_url: base_url.to_string(),
            mcp_url: Some("http://127.0.0.1:6789".to_string()),
            working_dir: PathBuf::from("/tmp/repo"),
            thread_id: None,
            started_at: Utc::now(),
            repo_id: None,
            worktree_id: None,
            workspace_id: None,
            lease_fence: None,
        }
    }

    fn scoped_control_info(pid: u32, scope: &ControlScope) -> ControlInfo {
        ControlInfo {
            version: CONTROL_INFO_VERSION,
            mode: "write".to_string(),
            pid,
            base_url: "http://127.0.0.1:3000".to_string(),
            mcp_url: None,
            working_dir: PathBuf::from("/tmp/repo"),
            thread_id: None,
            started_at: Utc::now(),
            repo_id: Some(scope.repo_id.clone()),
            worktree_id: scope.worktree_id.clone(),
            workspace_id: scope.workspace_id.clone(),
            lease_fence: scope.lease_fence,
        }
    }

    fn scope(repo: &str, worktree: Option<&str>) -> ControlScope {
        ControlScope {
            repo_id: repo.to_string(),
            worktree_id: worktree.map(str::to_string),
            workspace_id: None,
            lease_fence: None,
        }
    }

    #[tokio::test]
    async fn code_control_files_create_new_token_with_0600_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let token_path = temp.path().join("control-token");
        let lock_path = temp.path().join("control.lock");
        let guard = acquire_control_lock(&lock_path).unwrap();

        let token = ensure_control_token_file(&token_path).await.unwrap();

        assert!(!token.is_empty());
        assert_eq!(fs::read_to_string(&token_path).unwrap(), token);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let lock_contents = fs::read_to_string(&lock_path).unwrap();
        assert!(lock_contents.contains(&std::process::id().to_string()));
        assert!(!lock_contents.contains(&token));
        drop(guard);
    }

    #[tokio::test]
    async fn code_control_files_existing_0600_token_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let token_path = temp.path().join("control-token");
        fs::write(&token_path, "old-token").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&token_path, Permissions::from_mode(0o600)).unwrap();

        let new_token = ensure_control_token_file(&token_path).await.unwrap();

        assert_ne!(new_token, "old-token");
        assert_eq!(fs::read_to_string(&token_path).unwrap(), new_token);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn code_control_files_rejects_wide_token_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let token_path = temp.path().join("control-token");
        fs::write(&token_path, "old-token").unwrap();
        fs::set_permissions(&token_path, Permissions::from_mode(0o644)).unwrap();

        let error = ensure_control_token_file(&token_path).await.unwrap_err();

        assert!(error.to_string().contains("chmod 0600"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn code_control_files_rejects_symlink_token_path() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let token_path = temp.path().join("control-token");
        fs::write(&target, "target-content").unwrap();
        unix_fs::symlink(&target, &token_path).unwrap();

        let error = ensure_control_token_file(&token_path).await.unwrap_err();

        assert!(error.to_string().contains("must not be a symlink"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "target-content");
    }

    #[test]
    fn code_control_files_control_info_contains_no_token_material() {
        let info = test_control_info(12345, "http://127.0.0.1:3000");

        let json = serde_json::to_string(&info).unwrap();

        assert!(json.contains("baseUrl"));
        assert!(!json.contains("control-token"));
        assert!(!json.contains("token"));
        assert!(!json.contains("tokenHash"));
    }

    #[test]
    fn code_control_files_second_lock_fails_fast_with_live_instance() {
        let temp = tempfile::tempdir().unwrap();
        let lock_path = temp.path().join("control.lock");
        let info_path = temp.path().join("control.json");
        write_control_info(
            &info_path,
            &test_control_info(std::process::id(), "http://127.0.0.1:3000"),
        )
        .unwrap();

        let _guard = acquire_control_lock(&lock_path).unwrap();
        let error = acquire_control_lock(&lock_path).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("CONTROL_INSTANCE_CONFLICT"));
        assert!(message.contains(&std::process::id().to_string()));
        assert!(message.contains("http://127.0.0.1:3000"));
    }

    #[test]
    fn code_control_files_stale_info_does_not_block_lock() {
        let temp = tempfile::tempdir().unwrap();
        let lock_path = temp.path().join("control.lock");
        let info_path = temp.path().join("control.json");
        write_control_info(
            &info_path,
            &test_control_info(u32::MAX, "http://127.0.0.1:3000"),
        )
        .unwrap();

        assert!(inspect_existing_instance(&info_path).unwrap().is_none());
        let _guard = acquire_control_lock(&lock_path).unwrap();
    }

    #[test]
    fn code_control_files_custom_paths_have_independent_locks() {
        let temp = tempfile::tempdir().unwrap();
        let working_dir = temp.path().join("repo");
        let token_a = temp.path().join("a-token");
        let info_a = temp.path().join("a.json");
        let token_b = temp.path().join("b-token");
        let info_b = temp.path().join("b.json");

        let paths_a = resolve_control_paths(&working_dir, Some(&token_a), Some(&info_a));
        let paths_b = resolve_control_paths(&working_dir, Some(&token_b), Some(&info_b));

        assert_ne!(paths_a.lock, paths_b.lock);
        let _guard_a = acquire_control_lock(&paths_a.lock).unwrap();
        let _guard_b = acquire_control_lock(&paths_b.lock).unwrap();
    }

    #[test]
    fn code_control_files_pid_liveness_rejects_invalid_pid_values() {
        assert!(!pid_is_live(0));
        assert!(!pid_is_live(u32::MAX));
    }

    /// §C.12 named regression (plan-20260714 line 2759), UNIT half. `libra
    /// service` is a REPOSITORY-level sidecar, so its stale-file reclamation
    /// must be fenced by repository identity: a dead-PID control file
    /// belonging to ANOTHER repository is not this service's garbage to
    /// collect — its cleanup belongs to its own scope's owner. The
    /// same-repository / other-worktree case is deliberately NOT foreign for
    /// this policy (one service serves the whole repository), and that half
    /// is asserted too so the fence cannot be "tightened" into refusing
    /// legitimate reuse. The END-TO-END half — that `libra service run`
    /// actually consults this gate and stamps its own record — is
    /// `service_startup_refuses_a_foreign_stale_control_file` in
    /// tests/command/service_test.rs.
    #[test]
    fn service_stale_cleanup_does_not_touch_other_scope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let info_path = dir.path().join("service.json");

        // A DEAD-pid file written by a different repository's service.
        let foreign = scope("repo-other", None);
        let stale_foreign = scoped_control_info(u32::MAX, &foreign);
        std::fs::write(
            &info_path,
            serde_json::to_string(&stale_foreign).expect("serialize"),
        )
        .expect("write foreign control info");
        assert!(!pid_is_live(stale_foreign.pid), "the fixture pid is dead");

        let me = scope("repo-a", None);
        let error = ensure_scope_takeover_allowed(
            &info_path,
            &me,
            ControlScopePolicy::Repository,
            true,
            Path::new("/nonexistent-storage-for-this-unit-test"),
        )
        .expect_err("a foreign repository's stale control file is not ours to reclaim");
        assert!(
            matches!(error, ControlScopeError::Foreign { .. }),
            "the refusal names the scope mismatch: {error:?}"
        );
        assert!(
            info_path.exists(),
            "the refusal must leave the foreign file in place"
        );

        // ...while a stale file from ANOTHER WORKTREE of the SAME repository
        // is reclaimable: the service is repository-level by design.
        let sibling = scope("repo-a", Some("wt-sibling"));
        std::fs::write(
            &info_path,
            serde_json::to_string(&scoped_control_info(u32::MAX, &sibling)).expect("serialize"),
        )
        .expect("write sibling control info");
        ensure_scope_takeover_allowed(
            &info_path,
            &me,
            ControlScopePolicy::Repository,
            true,
            Path::new("/nonexistent-storage-for-this-unit-test"),
        )
        .expect("same repository, different worktree is the service's own scope");
    }

    #[test]
    fn scope_classification_v2_same_scope_matches() {
        let me = scope("repo-a", Some("wt-1"));
        let info = scoped_control_info(4242, &me);
        for policy in [ControlScopePolicy::Worktree, ControlScopePolicy::Repository] {
            assert_eq!(
                classify_control_scope(&info, &me, policy, true),
                ControlScopeCheck::Match
            );
        }
    }

    #[test]
    fn scope_classification_other_worktree_is_foreign_even_with_dead_pid_semantics() {
        // Classification is liveness-agnostic by design: a dead foreign
        // instance is just as untouchable as a live one.
        let owner = scope("repo-a", Some("wt-1"));
        let me = scope("repo-a", Some("wt-2"));
        let info = scoped_control_info(u32::MAX, &owner);
        assert_eq!(
            classify_control_scope(&info, &me, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::Foreign(ControlScopeMismatch::WorktreeId {
                found: Some("wt-1".to_string()),
                expected: Some("wt-2".to_string()),
            })
        );
        // A repository-level sidecar tolerates a different worktree of the
        // SAME repository (the advisory lock arbitrates liveness).
        assert_eq!(
            classify_control_scope(&info, &me, ControlScopePolicy::Repository, true),
            ControlScopeCheck::Match
        );
    }

    #[test]
    fn scope_classification_main_vs_linked_never_aliases() {
        let main = scope("repo-a", None);
        let linked = scope("repo-a", Some("wt-1"));
        let main_info = scoped_control_info(1, &main);
        assert!(matches!(
            classify_control_scope(&main_info, &linked, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::Foreign(ControlScopeMismatch::WorktreeId { .. })
        ));
        let linked_info = scoped_control_info(1, &linked);
        assert!(matches!(
            classify_control_scope(&linked_info, &main, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::Foreign(ControlScopeMismatch::WorktreeId { .. })
        ));
    }

    #[test]
    fn scope_classification_other_repository_is_foreign_under_both_policies() {
        let me = scope("repo-a", None);
        let info = scoped_control_info(1, &scope("repo-b", None));
        for policy in [ControlScopePolicy::Worktree, ControlScopePolicy::Repository] {
            assert_eq!(
                classify_control_scope(&info, &me, policy, false),
                ControlScopeCheck::Foreign(ControlScopeMismatch::RepoId {
                    found: "repo-b".to_string(),
                    expected: "repo-a".to_string(),
                })
            );
        }
    }

    #[test]
    fn scope_classification_workspace_and_fence_mismatch_are_foreign() {
        let mut owner = scope("repo-a", Some("wt-1"));
        owner.workspace_id = Some("ws-1".to_string());
        owner.lease_fence = Some(3);
        let info = scoped_control_info(1, &owner);

        let mut me = owner.clone();
        me.workspace_id = Some("ws-2".to_string());
        assert!(matches!(
            classify_control_scope(&info, &me, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::Foreign(ControlScopeMismatch::WorkspaceId { .. })
        ));

        let mut me = owner.clone();
        me.lease_fence = Some(4);
        assert!(matches!(
            classify_control_scope(&info, &me, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::Foreign(ControlScopeMismatch::LeaseFence { .. })
        ));

        // A side with NO workspace association is compatible with one that
        // has one — the mismatch rule only fires when both sides claim one.
        let me = scope("repo-a", Some("wt-1"));
        assert_eq!(
            classify_control_scope(&info, &me, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::Match
        );
    }

    #[test]
    fn scope_classification_legacy_follows_linked_evidence() {
        let me = scope("repo-a", None);
        let legacy = test_control_info(1, "http://127.0.0.1:3000");
        assert_eq!(
            classify_control_scope(&legacy, &me, ControlScopePolicy::Worktree, false),
            ControlScopeCheck::LegacyAdoptable
        );
        assert_eq!(
            classify_control_scope(&legacy, &me, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::LegacyAmbiguous
        );
        // A "version 2" file that lost its repoId is legacy, not trusted.
        let mut clipped = scoped_control_info(1, &me);
        clipped.repo_id = None;
        assert_eq!(
            classify_control_scope(&clipped, &me, ControlScopePolicy::Worktree, true),
            ControlScopeCheck::LegacyAmbiguous
        );
    }

    #[test]
    fn takeover_guard_matrix_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let info_path = temp.path().join("control.json");
        let me = scope("repo-a", Some("wt-2"));

        // Missing file: allowed.
        assert!(
            ensure_scope_takeover_allowed(
                &info_path,
                &me,
                ControlScopePolicy::Worktree,
                true,
                Path::new("/nonexistent-storage-for-this-unit-test"),
            )
            .is_ok()
        );

        // Foreign worktree, dead pid: refused, and the file must survive.
        let owner = scope("repo-a", Some("wt-1"));
        write_control_info(&info_path, &scoped_control_info(u32::MAX, &owner)).unwrap();
        let error = ensure_scope_takeover_allowed(
            &info_path,
            &me,
            ControlScopePolicy::Worktree,
            true,
            Path::new("/nonexistent-storage-for-this-unit-test"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("CONTROL_SCOPE_CONFLICT"));
        assert!(info_path.exists());

        // Same scope: allowed.
        write_control_info(&info_path, &scoped_control_info(u32::MAX, &me)).unwrap();
        assert!(
            ensure_scope_takeover_allowed(
                &info_path,
                &me,
                ControlScopePolicy::Worktree,
                true,
                Path::new("/nonexistent-storage-for-this-unit-test"),
            )
            .is_ok()
        );

        // Legacy + linked evidence: refused; without evidence: allowed.
        fs::write(
            &info_path,
            serde_json::to_string(&test_control_info(1, "http://127.0.0.1:3000")).unwrap(),
        )
        .unwrap();
        assert!(
            ensure_scope_takeover_allowed(
                &info_path,
                &me,
                ControlScopePolicy::Worktree,
                true,
                Path::new("/nonexistent-storage-for-this-unit-test"),
            )
            .is_err()
        );
        assert!(
            ensure_scope_takeover_allowed(
                &info_path,
                &me,
                ControlScopePolicy::Worktree,
                false,
                Path::new("/nonexistent-storage-for-this-unit-test"),
            )
            .is_ok()
        );

        // Malformed + linked evidence: refused; without evidence: replaceable.
        fs::write(&info_path, "{not json").unwrap();
        assert!(
            ensure_scope_takeover_allowed(
                &info_path,
                &me,
                ControlScopePolicy::Worktree,
                true,
                Path::new("/nonexistent-storage-for-this-unit-test"),
            )
            .is_err()
        );
        assert!(
            ensure_scope_takeover_allowed(
                &info_path,
                &me,
                ControlScopePolicy::Worktree,
                false,
                Path::new("/nonexistent-storage-for-this-unit-test"),
            )
            .is_ok()
        );
    }

    #[test]
    fn cleanup_skips_info_file_recorded_by_another_process() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ControlPaths {
            token: temp.path().join("control-token"),
            info: temp.path().join("control.json"),
            lock: temp.path().join("control.lock"),
        };
        // Another process's pid in the info file: cleanup must leave it.
        write_control_info(
            &paths.info,
            &test_control_info(std::process::id() + 1, "http://127.0.0.1:3000"),
        )
        .unwrap();
        cleanup_control_files(&paths, false, true);
        assert!(paths.info.exists());

        // Our own pid: cleanup removes it.
        write_control_info(
            &paths.info,
            &test_control_info(std::process::id(), "http://127.0.0.1:3000"),
        )
        .unwrap();
        cleanup_control_files(&paths, false, true);
        assert!(!paths.info.exists());
    }

    #[test]
    fn v1_control_info_json_still_deserializes() {
        // A pre-W4 file has none of the scope keys; parsing must not break.
        let json = r#"{
            "version": 1,
            "mode": "write",
            "pid": 4242,
            "baseUrl": "http://127.0.0.1:3000",
            "workingDir": "/tmp/repo",
            "startedAt": "2026-07-01T00:00:00Z"
        }"#;
        let info: ControlInfo = serde_json::from_str(json).unwrap();
        assert!(!info.has_scope());
        assert_eq!(info.repo_id, None);
        assert_eq!(info.worktree_id, None);
    }

    #[test]
    fn control_lock_error_display_pins_owned_variants() {
        let with_existing = ControlLockError::AlreadyHeld {
            existing: Some(LiveInstanceInfo {
                pid: 4242,
                base_url: Some("http://127.0.0.1:6788".to_string()),
                started_at: None,
            }),
            info_path: PathBuf::from("/tmp/control.json"),
            lock_path: PathBuf::from("/tmp/control.lock"),
        };
        assert_eq!(
            with_existing.to_string(),
            "CONTROL_INSTANCE_CONFLICT: another `libra code --control write` instance is active \
             (pid: 4242, baseUrl: http://127.0.0.1:6788). info: /tmp/control.json, \
             lock: /tmp/control.lock. Stop the existing instance (Ctrl-C / kill 4242) or pass \
             `--control-token-file` and `--control-info-file` to use separate paths.",
        );

        let with_existing_no_url = ControlLockError::AlreadyHeld {
            existing: Some(LiveInstanceInfo {
                pid: 1234,
                base_url: None,
                started_at: None,
            }),
            info_path: PathBuf::from("/tmp/control.json"),
            lock_path: PathBuf::from("/tmp/control.lock"),
        };
        assert_eq!(
            with_existing_no_url.to_string(),
            "CONTROL_INSTANCE_CONFLICT: another `libra code --control write` instance is active \
             (pid: 1234). info: /tmp/control.json, lock: /tmp/control.lock. \
             Stop the existing instance (Ctrl-C / kill 1234) or pass `--control-token-file` and \
             `--control-info-file` to use separate paths.",
        );

        let without_existing = ControlLockError::AlreadyHeld {
            existing: None,
            info_path: PathBuf::from("/tmp/control.json"),
            lock_path: PathBuf::from("/tmp/control.lock"),
        };
        assert_eq!(
            without_existing.to_string(),
            "CONTROL_INSTANCE_CONFLICT: another `libra code --control write` instance holds the \
             control lock. info: /tmp/control.json, lock: /tmp/control.lock. Stop the existing \
             instance or pass `--control-token-file` and `--control-info-file` to use separate \
             paths.",
        );
    }
}
