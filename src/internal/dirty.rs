//! Dirty-set cache (lore.md §1.1) — the SINGLE owner API for the
//! `working_dirty` / `working_dirty_meta` tables.
//!
//! The cache is an ADVISORY snapshot: default `libra status` never reads or
//! writes it, and no correctness decision anywhere may depend on it. The
//! opt-in surfaces are `status --scan` (the only authoritative rebuild),
//! `status --cached` (consume), `status --check-dirty` (re-verify the cached
//! set), and `libra dirty` (manual advisory marks — over-reporting is the
//! safe direction).
//!
//! FRESHNESS is keyed on two facts captured at scan time: the index file's
//! trailing content checksum (`index_fingerprint` — O(1) to recompute,
//! immune to mtime-granularity races) and the HEAD commit OID (the staged
//! snapshot is an index↔HEAD fact). Any consumer that finds a mismatch, a
//! missing meta row, or `state='stale'` MUST degrade to the full reconcile.
//! Every index- or HEAD-mutating command therefore invalidates the cache
//! implicitly and for free (lore.md §7.1.1's fallback clause: no cross-domain
//! index-file/SQLite atomicity exists, so v1 does not attempt per-command
//! carry-over). SNAPSHOT SEMANTICS: worktree-only edits made AFTER the scan
//! do not change the fingerprint and are by design invisible to `--cached`
//! until a rescan or a `libra dirty` mark records them — that is precisely
//! what the advisory marks are for (tooling marks paths as it edits). Within
//! the recorded facts the cache may over-report (manual marks, later-pruned
//! rows) but never under-reports what a scan or mark recorded.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, sea_query::OnConflict,
};

use crate::internal::{
    db::get_db_conn_instance,
    model::{working_dirty, working_dirty_meta},
};

/// Row kinds for the unstaged dirty set.
pub const KIND_NEW: &str = "new";
pub const KIND_MODIFIED: &str = "modified";
pub const KIND_DELETED: &str = "deleted";
/// Row kinds for the staged snapshot (index↔HEAD at scan time).
pub const KIND_STAGED_NEW: &str = "staged_new";
pub const KIND_STAGED_MODIFIED: &str = "staged_modified";
pub const KIND_STAGED_DELETED: &str = "staged_deleted";
/// Manual `libra dirty` marks — classified in memory at read time.
pub const KIND_UNKNOWN: &str = "unknown";

pub const SOURCE_SCAN: &str = "scan";
pub const SOURCE_MANUAL: &str = "manual";
pub const SOURCE_CHECK: &str = "check";

/// Steal a scan lock older than this many seconds (a crashed scanner must not
/// wedge the cache forever; a genuinely long scan may then race, which is
/// last-writer-wins over a consistent snapshot — documented).
pub const SCAN_LOCK_STEAL_SECS: i64 = 600;

/// Sentinel fingerprint recorded when no index file exists.
pub const FINGERPRINT_ABSENT: &str = "absent";

/// FIXED-WIDTH UTC timestamp (RFC3339, microsecond precision) — every stored
/// timestamp and cutoff comparison in this module goes through here so
/// lexicographic string comparison is exactly chronological (variable-width
/// fractions would make `<` unsound at tick boundaries).
pub fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyEntry {
    pub path: String,
    pub kind: String,
    pub source: String,
    pub marked_at: String,
    pub verified_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyMeta {
    pub state: String,
    pub index_fingerprint: Option<String>,
    pub head_oid: Option<String>,
    pub scanned_at: Option<String>,
    pub scan_lock_pid: Option<i64>,
    pub scan_lock_at: Option<String>,
}

/// The cache's freshness as seen by a consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    Fresh,
    Stale,
    Missing,
}

impl CacheState {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheState::Fresh => "fresh",
            CacheState::Stale => "stale",
            CacheState::Missing => "missing",
        }
    }
}

/// Normalize a repo-relative path for storage: '/'-separated on every
/// platform (round-trips through [`stored_path_to_native`]). STRICT: a
/// non-UTF-8 path is an error, never lossy-converted — a replacement-character
/// mangle would be re-read as a DIFFERENT (valid) path and could be pruned or
/// misverified. This matches the status pipeline's own contract
/// (`StatusError::InvalidPathEncoding`): undecodable paths never reach the
/// cache, they fail loudly upstream.
pub fn native_path_to_stored(path: &Path) -> Result<String> {
    let text = path.to_str().with_context(|| {
        format!(
            "path {:?} is not valid UTF-8 and cannot be recorded in the dirty cache",
            path.display()
        )
    })?;
    Ok(if std::path::MAIN_SEPARATOR == '/' {
        text.to_string()
    } else {
        text.replace(std::path::MAIN_SEPARATOR, "/")
    })
}

/// Inverse of [`native_path_to_stored`].
pub fn stored_path_to_native(stored: &str) -> std::path::PathBuf {
    if std::path::MAIN_SEPARATOR == '/' {
        std::path::PathBuf::from(stored)
    } else {
        std::path::PathBuf::from(stored.replace('/', std::path::MAIN_SEPARATOR_STR))
    }
}

/// The index file's trailing content checksum as lowercase hex — the O(1)
/// freshness fingerprint. The Git index format ends with a checksum of the
/// preceding content whose width follows the active hash kind; reading the
/// tail is race-robust in a way stat/mtime epochs are not. Returns
/// [`FINGERPRINT_ABSENT`] when the index file does not exist.
pub fn current_index_fingerprint(index_path: &Path) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let width = match git_internal::hash::get_hash_kind() {
        git_internal::hash::HashKind::Sha1 => 20usize,
        git_internal::hash::HashKind::Sha256 => 32usize,
    };
    let mut file = match std::fs::File::open(index_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FINGERPRINT_ABSENT.to_string());
        }
        Err(error) => {
            return Err(error).context("failed to open the index for fingerprinting");
        }
    };
    let len = file
        .metadata()
        .context("failed to stat the index for fingerprinting")?
        .len();
    if (len as usize) < width {
        // Truncated/corrupt index: fingerprint on what exists so a rewrite
        // still changes the value.
        return Ok(format!("short:{len}"));
    }
    file.seek(SeekFrom::End(-(width as i64)))
        .context("failed to seek the index trailing checksum")?;
    let mut tail = vec![0u8; width];
    file.read_exact(&mut tail)
        .context("failed to read the index trailing checksum")?;
    Ok(tail.iter().map(|b| format!("{b:02x}")).collect())
}

/// Validate and normalize repo-relative candidate paths for advisory marks —
/// the SHARED gate for every mark producer (CLI, service endpoint, watcher).
/// Rejects absolute paths and any parent/root/prefix component (an escaping
/// stored path would make later verification stat/hash files OUTSIDE the
/// repository), and refuses non-UTF-8 (never lossy-mangled). Returns the
/// '/'-normalized stored forms, or the list of offending inputs.
pub fn validate_mark_paths(
    workdir_relative: &[std::path::PathBuf],
) -> std::result::Result<Vec<String>, Vec<String>> {
    let mut stored = Vec::with_capacity(workdir_relative.len());
    let mut offenders = Vec::new();
    for path in workdir_relative {
        let escapes = path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
        if escapes {
            offenders.push(path.display().to_string());
            continue;
        }
        match native_path_to_stored(path) {
            Ok(text) => stored.push(text),
            Err(_) => offenders.push(path.display().to_string()),
        }
    }
    if offenders.is_empty() {
        Ok(stored)
    } else {
        Err(offenders)
    }
}

/// The single owner API for the dirty-set cache tables.
pub struct DirtyCache;

impl DirtyCache {
    /// This worktree's scope key for the dirty-cache tables (`""` = main
    /// worktree; plan-20260714 §C.4.1.1).
    fn scope_key() -> String {
        crate::internal::worktree_scope::WorktreeScope::current()
            .storage_key()
            .to_string()
    }
    /// The meta row, when the cache has ever been touched.
    pub async fn meta_with_conn<C: ConnectionTrait>(db: &C) -> Result<Option<DirtyMeta>> {
        // Resolve the worktree scope ONCE per request (worktree_scope.rs
        // contract): every statement below uses this same value.
        let scope = Self::scope_key();
        let row = working_dirty_meta::Entity::find_by_id(scope.clone())
            .one(db)
            .await
            .context("failed to read working_dirty_meta")?;
        Ok(row.map(|row| DirtyMeta {
            state: row.state,
            index_fingerprint: row.index_fingerprint,
            head_oid: row.head_oid,
            scanned_at: row.scanned_at,
            scan_lock_pid: row.scan_lock_pid,
            scan_lock_at: row.scan_lock_at,
        }))
    }

    pub async fn meta() -> Result<Option<DirtyMeta>> {
        let db = get_db_conn_instance().await;
        Self::meta_with_conn(&db).await
    }

    /// Classify freshness against the CURRENT fingerprint + HEAD.
    pub fn classify(
        meta: Option<&DirtyMeta>,
        current_fingerprint: &str,
        current_head: Option<&str>,
    ) -> CacheState {
        match meta {
            None => CacheState::Missing,
            Some(meta) => {
                let fingerprint_ok = meta.index_fingerprint.as_deref() == Some(current_fingerprint);
                let head_ok = meta.head_oid.as_deref() == current_head;
                if meta.state == "fresh" && fingerprint_ok && head_ok {
                    CacheState::Fresh
                } else {
                    CacheState::Stale
                }
            }
        }
    }

    /// All cached rows, path-ordered.
    pub async fn list_with_conn<C: ConnectionTrait>(db: &C) -> Result<Vec<DirtyEntry>> {
        // Resolve the worktree scope ONCE per request (worktree_scope.rs
        // contract): every statement below uses this same value.
        let scope = Self::scope_key();
        let rows = working_dirty::Entity::find()
            .filter(working_dirty::Column::WorktreeId.eq(scope.clone()))
            .order_by_asc(working_dirty::Column::Path)
            .order_by_asc(working_dirty::Column::Kind)
            .all(db)
            .await
            .context("failed to list working_dirty")?;
        Ok(rows
            .into_iter()
            .map(|row| DirtyEntry {
                path: row.path,
                kind: row.kind,
                source: row.source,
                marked_at: row.marked_at,
                verified_at: row.verified_at,
            })
            .collect())
    }

    pub async fn list() -> Result<Vec<DirtyEntry>> {
        let db = get_db_conn_instance().await;
        Self::list_with_conn(&db).await
    }

    /// Upsert manual advisory marks (`libra dirty <paths>`, the service mark
    /// endpoint, the watcher). Over-reporting is the safe direction, so marks
    /// never invalidate the snapshot epoch. Validation is ENFORCED here — the
    /// public marking entrypoint takes workdir-relative native paths and
    /// refuses the whole batch if any escapes the repository (an escaping
    /// stored path would make later verification stat/hash files OUTSIDE the
    /// repo). Returns the stored forms that were marked.
    pub async fn mark_paths_with_conn<C: ConnectionTrait>(
        db: &C,
        workdir_relative: &[std::path::PathBuf],
    ) -> std::result::Result<Vec<String>, MarkError> {
        let stored_paths = validate_mark_paths(workdir_relative).map_err(MarkError::Escaping)?;
        Self::mark_stored_paths_with_conn(db, &stored_paths)
            .await
            .map_err(MarkError::Store)?;
        Ok(stored_paths)
    }

    pub async fn mark_paths(
        workdir_relative: &[std::path::PathBuf],
    ) -> std::result::Result<Vec<String>, MarkError> {
        let db = get_db_conn_instance().await;
        Self::mark_paths_with_conn(&db, workdir_relative).await
    }

    /// Raw insertion for PRE-VALIDATED stored paths (private: every public
    /// entry goes through [`validate_mark_paths`]).
    async fn mark_stored_paths_with_conn<C: ConnectionTrait>(
        db: &C,
        stored_paths: &[String],
    ) -> Result<()> {
        let scope = Self::scope_key();
        let now = now_timestamp();
        for path in stored_paths {
            let active = working_dirty::ActiveModel {
                worktree_id: Set(scope.clone()),
                path: Set(path.clone()),
                kind: Set(KIND_UNKNOWN.to_string()),
                source: Set(SOURCE_MANUAL.to_string()),
                marked_at: Set(now.clone()),
                verified_at: Set(None),
                ..Default::default()
            };
            working_dirty::Entity::insert(active)
                .on_conflict(
                    OnConflict::columns([
                        working_dirty::Column::WorktreeId,
                        working_dirty::Column::Path,
                        working_dirty::Column::Kind,
                    ])
                    .update_columns([
                        working_dirty::Column::Source,
                        working_dirty::Column::MarkedAt,
                    ])
                    .to_owned(),
                )
                .exec(db)
                .await
                .context("failed to upsert a manual dirty mark")?;
        }
        Ok(())
    }

    /// Atomically replace the snapshot (the `--scan` commit step): scan rows
    /// and PRE-SCAN advisory marks are deleted (the reconcile subsumed them),
    /// the given rows inserted, and the meta row stamped fresh — call INSIDE
    /// one transaction so `--cached` readers see either the old or the new
    /// snapshot, never a half-update. Advisory marks recorded AFTER
    /// `scan_started_at` survive: a mark landing between the reconcile's walk
    /// and this commit must not be wiped (that would under-report a recorded
    /// fact; surviving marks can only over-report — the safe direction).
    pub async fn replace_all_with_conn<C: ConnectionTrait>(
        db: &C,
        entries: &[(String, &'static str)],
        fingerprint: &str,
        head_oid: Option<&str>,
        scan_started_at: &str,
    ) -> Result<()> {
        // Resolve the worktree scope ONCE per request (worktree_scope.rs
        // contract): every statement below uses this same value.
        let scope = Self::scope_key();
        let now = now_timestamp();
        working_dirty::Entity::delete_many()
            .filter(working_dirty::Column::WorktreeId.eq(scope.clone()))
            .filter(
                sea_orm::Condition::any()
                    .add(working_dirty::Column::Source.eq(SOURCE_SCAN))
                    .add(working_dirty::Column::Source.eq(SOURCE_CHECK))
                    // STRICT '<': a mark stamped in the same microsecond as
                    // the scan start survives (over-report only — wiping it
                    // would recreate the under-report race).
                    .add(working_dirty::Column::MarkedAt.lt(scan_started_at)),
            )
            .exec(db)
            .await
            .context("failed to clear working_dirty for the scan snapshot")?;
        for (path, kind) in entries {
            let active = working_dirty::ActiveModel {
                worktree_id: Set(scope.clone()),
                path: Set(path.clone()),
                kind: Set((*kind).to_string()),
                source: Set(SOURCE_SCAN.to_string()),
                marked_at: Set(now.clone()),
                verified_at: Set(Some(now.clone())),
                ..Default::default()
            };
            // A surviving post-scan-start mark may collide on (path, kind):
            // upgrade it to the scan row (same fact, now verified).
            working_dirty::Entity::insert(active)
                .on_conflict(
                    OnConflict::columns([
                        working_dirty::Column::WorktreeId,
                        working_dirty::Column::Path,
                        working_dirty::Column::Kind,
                    ])
                    .update_columns([
                        working_dirty::Column::Source,
                        working_dirty::Column::MarkedAt,
                        working_dirty::Column::VerifiedAt,
                    ])
                    .to_owned(),
                )
                .exec(db)
                .await
                .context("failed to insert a scan snapshot row")?;
        }
        Self::upsert_meta_with_conn(db, &scope, "fresh", Some(fingerprint), head_oid, Some(&now))
            .await
    }

    /// Mark the cache stale (kept for future per-command carry-over hooks).
    pub async fn mark_stale_with_conn<C: ConnectionTrait>(db: &C) -> Result<()> {
        // Resolve the worktree scope ONCE per request (worktree_scope.rs
        // contract). This deliberately does NOT route through
        // `upsert_meta_with_conn`: staleness only FLIPS an existing row —
        // creating a meta row here would fabricate freshness metadata.
        let scope = Self::scope_key();
        if let Some(row) = working_dirty_meta::Entity::find_by_id(scope.clone())
            .one(db)
            .await
            .context("failed to read working_dirty_meta")?
        {
            let mut active: working_dirty_meta::ActiveModel = row.into();
            active.state = Set("stale".to_string());
            active
                .update(db)
                .await
                .context("failed to mark the dirty cache stale")?;
        }
        Ok(())
    }

    async fn upsert_meta_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: &str,
        state: &str,
        fingerprint: Option<&str>,
        head_oid: Option<&str>,
        scanned_at: Option<&str>,
    ) -> Result<()> {
        let existing = working_dirty_meta::Entity::find_by_id(scope.to_string())
            .one(db)
            .await
            .context("failed to read working_dirty_meta")?;
        match existing {
            Some(row) => {
                let mut active: working_dirty_meta::ActiveModel = row.into();
                active.state = Set(state.to_string());
                active.index_fingerprint = Set(fingerprint.map(str::to_string));
                active.head_oid = Set(head_oid.map(str::to_string));
                active.scanned_at = Set(scanned_at.map(str::to_string));
                active
                    .update(db)
                    .await
                    .context("failed to update working_dirty_meta")?;
            }
            None => {
                let active = working_dirty_meta::ActiveModel {
                    worktree_id: Set(scope.to_string()),
                    state: Set(state.to_string()),
                    index_fingerprint: Set(fingerprint.map(str::to_string)),
                    head_oid: Set(head_oid.map(str::to_string)),
                    scanned_at: Set(scanned_at.map(str::to_string)),
                    scan_lock_pid: Set(None),
                    scan_lock_at: Set(None),
                };
                active
                    .insert(db)
                    .await
                    .context("failed to seed working_dirty_meta")?;
            }
        }
        Ok(())
    }

    /// Try to take the scan lock. Seeds the meta row when absent. A lock older
    /// than [`SCAN_LOCK_STEAL_SECS`] is stolen (with `stole = true` so the
    /// caller can warn). PID-based locking is best-effort (documented: PID
    /// reuse and network filesystems weaken it; a raced scan is
    /// last-writer-wins over a consistent snapshot, never a half-update).
    pub async fn try_acquire_scan_lock_with_conn<C: ConnectionTrait>(
        db: &C,
        pid: i64,
    ) -> Result<ScanLockOutcome> {
        // Resolve the worktree scope ONCE per request (worktree_scope.rs
        // contract): every statement below uses this same value.
        let scope = Self::scope_key();
        use sea_orm::sea_query::Expr;
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        // Seed the single meta row if absent (ON CONFLICT DO NOTHING), then
        // take the lock with one conditional UPDATE — a real compare-and-swap
        // (predicate: unlocked, or the held lock is stale/unparseable). All
        // timestamps are UTC RFC3339, so lexicographic comparison is
        // chronological.
        let seed = working_dirty_meta::ActiveModel {
            worktree_id: Set(scope.clone()),
            state: Set("stale".to_string()),
            index_fingerprint: Set(None),
            head_oid: Set(None),
            scanned_at: Set(None),
            scan_lock_pid: Set(None),
            scan_lock_at: Set(None),
        };
        working_dirty_meta::Entity::insert(seed)
            .on_conflict(
                OnConflict::column(working_dirty_meta::Column::WorktreeId)
                    .do_nothing()
                    .to_owned(),
            )
            .do_nothing()
            .exec(db)
            .await
            .context("failed to seed working_dirty_meta")?;
        let cutoff = (now - chrono::Duration::seconds(SCAN_LOCK_STEAL_SECS)).to_rfc3339();
        // Detect (best-effort, pre-CAS) whether we would be stealing, purely
        // for the warning; the CAS itself is authoritative.
        let held_before = working_dirty_meta::Entity::find_by_id(scope.clone())
            .one(db)
            .await
            .context("failed to read working_dirty_meta")?
            .and_then(|row| row.scan_lock_pid);
        let result = working_dirty_meta::Entity::update_many()
            .col_expr(working_dirty_meta::Column::ScanLockPid, Expr::value(pid))
            .col_expr(
                working_dirty_meta::Column::ScanLockAt,
                Expr::value(now_text),
            )
            .filter(working_dirty_meta::Column::WorktreeId.eq(scope.clone()))
            .filter(
                sea_orm::Condition::any()
                    .add(working_dirty_meta::Column::ScanLockPid.is_null())
                    .add(working_dirty_meta::Column::ScanLockAt.is_null())
                    .add(working_dirty_meta::Column::ScanLockAt.lt(cutoff.clone())),
            )
            .exec(db)
            .await
            .context("failed to take the scan lock")?;
        if result.rows_affected == 1 {
            return Ok(ScanLockOutcome::Acquired {
                stole: held_before.is_some(),
            });
        }
        // Lost the CAS: report the current holder.
        let row = working_dirty_meta::Entity::find_by_id(scope.clone())
            .one(db)
            .await
            .context("failed to read working_dirty_meta")?;
        match row {
            Some(row) => Ok(ScanLockOutcome::Held {
                pid: row.scan_lock_pid.unwrap_or_default(),
                since: row.scan_lock_at.unwrap_or_default(),
            }),
            None => Ok(ScanLockOutcome::Held {
                pid: 0,
                since: String::new(),
            }),
        }
    }

    /// Release the scan lock (best-effort; only clears our own pid).
    pub async fn release_scan_lock_with_conn<C: ConnectionTrait>(db: &C, pid: i64) -> Result<()> {
        // Resolve the worktree scope ONCE per request (worktree_scope.rs
        // contract): every statement below uses this same value.
        let scope = Self::scope_key();
        if let Some(row) = working_dirty_meta::Entity::find_by_id(scope.clone())
            .one(db)
            .await
            .context("failed to read working_dirty_meta")?
            && row.scan_lock_pid == Some(pid)
        {
            let mut active: working_dirty_meta::ActiveModel = row.into();
            active.scan_lock_pid = Set(None);
            active.scan_lock_at = Set(None);
            active
                .update(db)
                .await
                .context("failed to release the scan lock")?;
        }
        Ok(())
    }

    /// Remove rows re-verified as clean and stamp survivors (`--check-dirty`).
    pub async fn prune_and_confirm_with_conn<C: ConnectionTrait>(
        db: &C,
        pruned: &[(String, String)],
        confirmed: &[(String, String)],
    ) -> Result<()> {
        // Resolve the worktree scope ONCE per request (worktree_scope.rs
        // contract): every statement below uses this same value.
        let scope = Self::scope_key();
        let now = now_timestamp();
        for (path, kind) in pruned {
            working_dirty::Entity::delete_many()
                .filter(working_dirty::Column::WorktreeId.eq(scope.clone()))
                .filter(working_dirty::Column::Path.eq(path.as_str()))
                .filter(working_dirty::Column::Kind.eq(kind.as_str()))
                .exec(db)
                .await
                .context("failed to prune a re-verified clean row")?;
        }
        for (path, kind) in confirmed {
            working_dirty::Entity::update_many()
                .filter(working_dirty::Column::WorktreeId.eq(scope.clone()))
                .col_expr(
                    working_dirty::Column::VerifiedAt,
                    sea_orm::sea_query::Expr::value(Some(now.clone())),
                )
                .col_expr(
                    working_dirty::Column::Source,
                    sea_orm::sea_query::Expr::value(SOURCE_CHECK),
                )
                .filter(working_dirty::Column::Path.eq(path.as_str()))
                .filter(working_dirty::Column::Kind.eq(kind.as_str()))
                .exec(db)
                .await
                .context("failed to stamp a confirmed dirty row")?;
        }
        Ok(())
    }
}

/// Error from the validated marking entrypoint.
#[derive(Debug)]
pub enum MarkError {
    /// One or more inputs escape the repository (whole batch refused).
    Escaping(Vec<String>),
    Store(anyhow::Error),
}

impl std::fmt::Display for MarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarkError::Escaping(offenders) => {
                write!(
                    f,
                    "paths escape the repository root: {}",
                    offenders.join(", ")
                )
            }
            MarkError::Store(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for MarkError {}

/// Outcome of a scan-lock acquisition attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanLockOutcome {
    Acquired { stole: bool },
    Held { pid: i64, since: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_storage_round_trips() {
        let native = std::path::PathBuf::from("dir").join("file.txt");
        let stored = native_path_to_stored(&native).expect("utf-8 path");
        assert_eq!(stored, "dir/file.txt");
        assert_eq!(stored_path_to_native(&stored), native);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_refused_not_mangled() {
        use std::os::unix::ffi::OsStrExt;
        let bad = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"f\xff.txt"));
        assert!(
            native_path_to_stored(&bad).is_err(),
            "lossy conversion would let the row be re-read as a different path"
        );
    }

    #[test]
    fn validate_mark_paths_matrix() {
        use std::path::PathBuf;
        let ok = validate_mark_paths(&[PathBuf::from("a.txt"), PathBuf::from("d/b.txt")])
            .expect("relative paths accepted");
        assert_eq!(ok, vec!["a.txt".to_string(), "d/b.txt".to_string()]);
        for bad in ["../x", "/etc/hosts", "a/../../x"] {
            let err = validate_mark_paths(&[PathBuf::from(bad)]);
            assert!(err.is_err(), "{bad} must be rejected");
        }
        // One offender fails the whole batch (atomic refusal).
        let err = validate_mark_paths(&[PathBuf::from("ok.txt"), PathBuf::from("../bad")]);
        assert_eq!(err.unwrap_err(), vec!["../bad".to_string()]);
    }

    #[test]
    fn classify_matrix() {
        let meta = DirtyMeta {
            state: "fresh".to_string(),
            index_fingerprint: Some("abc".to_string()),
            head_oid: Some("h1".to_string()),
            scanned_at: None,
            scan_lock_pid: None,
            scan_lock_at: None,
        };
        assert_eq!(
            DirtyCache::classify(Some(&meta), "abc", Some("h1")),
            CacheState::Fresh
        );
        // Any mismatch — fingerprint, HEAD, or explicit stale — is Stale.
        assert_eq!(
            DirtyCache::classify(Some(&meta), "zzz", Some("h1")),
            CacheState::Stale
        );
        assert_eq!(
            DirtyCache::classify(Some(&meta), "abc", Some("h2")),
            CacheState::Stale
        );
        assert_eq!(
            DirtyCache::classify(Some(&meta), "abc", None),
            CacheState::Stale
        );
        let stale = DirtyMeta {
            state: "stale".to_string(),
            ..meta.clone()
        };
        assert_eq!(
            DirtyCache::classify(Some(&stale), "abc", Some("h1")),
            CacheState::Stale
        );
        assert_eq!(DirtyCache::classify(None, "abc", None), CacheState::Missing);
    }
}
