//! Worktree dirty-cache status modes.

use super::*;

pub(super) async fn effective_ignore_case_for_status() -> CliResult<bool> {
    crate::utils::path_case::effective_ignore_case()
        .await
        .map_err(|error| {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
        })
}

fn dirty_cache_error(action: &str, error: anyhow::Error) -> CliError {
    CliError::fatal(format!("failed to {action} the dirty cache: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

/// Snapshot rows from the raw sets ('/'-normalized repo-relative paths).
/// Cache rows for a snapshot, plus the count of paths that could not be
/// represented in the cache at all.
struct SnapshotRows {
    rows: Vec<(String, &'static str)>,
    unencodable: u64,
}

fn snapshot_rows(staged: &Changes, unstaged: &Changes) -> SnapshotRows {
    use crate::internal::dirty;
    let mut rows: Vec<(String, &'static str)> = Vec::new();
    let mut unencodable = 0u64;
    let mut push = |paths: &[PathBuf], kind: &'static str| {
        for path in paths {
            // A non-UTF-8 name cannot be stored, but §B.6.1 forbids failing
            // status over one: the row is DROPPED (never lossy-mangled into
            // a different file's key) and the omission is reported, so
            // `--cached` under-reporting that path is visible rather than
            // silent.
            match dirty::native_path_to_stored(path) {
                Ok(stored) => rows.push((stored, kind)),
                Err(_) => unencodable += 1,
            }
        }
    };
    push(&unstaged.new, dirty::KIND_NEW);
    push(&unstaged.modified, dirty::KIND_MODIFIED);
    push(&unstaged.deleted, dirty::KIND_DELETED);
    push(&staged.new, dirty::KIND_STAGED_NEW);
    push(&staged.modified, dirty::KIND_STAGED_MODIFIED);
    push(&staged.deleted, dirty::KIND_STAGED_DELETED);
    SnapshotRows { rows, unencodable }
}

/// `status --scan`: run the full safe reconcile and atomically replace the
/// cache snapshot from it. TOCTOU-safe: the index fingerprint and HEAD are
/// captured BEFORE the reconcile and re-verified AFTER — a concurrent index
/// writer aborts the cache commit (the old snapshot stays intact) instead of
/// stamping rows computed against an older index as fresh.
pub(super) async fn run_status_scan(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    output: &OutputConfig,
) -> CliResult<()> {
    use crate::internal::dirty::{DirtyCache, ScanLockOutcome};

    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;
    let mut cache_warnings: Vec<StatusWarning> = Vec::new();
    let pid = std::process::id() as i64;
    match DirtyCache::try_acquire_scan_lock_with_conn(&db, pid)
        .await
        .map_err(|e| dirty_cache_error("lock", e))?
    {
        ScanLockOutcome::Acquired { stole } => {
            if stole {
                cache_warnings.push(cache_warning(
                    StatusWarningCode::DirtyCacheLockStolen,
                    "stole a stale dirty-cache scan lock (previous scanner crashed?)",
                ));
            }
        }
        ScanLockOutcome::Held { pid, since } => {
            return Err(CliError::failure(format!(
                "another `status --scan` holds the dirty-cache lock (pid {pid}, since {since})"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint("wait for it to finish, or re-run later (stale locks are stolen)"));
        }
    }
    // Everything below must release the lock — including error paths.
    let result = run_status_scan_locked(args, output, &index_path, extras, cache_warnings).await;
    let _ = DirtyCache::release_scan_lock_with_conn(&db, pid).await;
    result?;
    // Re-open a plain connection for the final read in JSON mode is not
    // needed; run_status_scan_locked rendered already.
    Ok(())
}

async fn run_status_scan_locked(
    args: &StatusArgs,
    output: &OutputConfig,
    index_path: &std::path::Path,
    extras: StatusConfigExtras,
    mut cache_warnings: Vec<StatusWarning>,
) -> CliResult<()> {
    // Preserve the stolen-lock diagnostic on EVERY failure inside the locked
    // scan (fingerprints, raw sets, collection, cache txn, even a failed
    // JSON emit after the payload append) — parity with the pre-R0-8b
    // immediate stderr warning. The snapshot makes delivery independent of
    // where the failure lands; JSON mode also gets the stderr line on ERROR
    // paths only (the success-path matrix — clean stderr — is untouched,
    // and losing the diagnostic entirely would be worse than annotating an
    // already-broken envelope exchange). Success delivers exactly once via
    // the payload.
    let result =
        run_status_scan_locked_inner(args, output, index_path, extras, &mut cache_warnings).await;
    if result.as_ref().is_err_and(|error| !error.is_silent()) {
        // Silent exits skip the fallback BY DESIGN: (a) the 9≻1 arbitration
        // already delivered via the rendered payload; (b) a stdout
        // BrokenPipe maps to a silent exit whose P0-06 contract
        // (`compat_broken_pipe_output`) pins stderr to ZERO noise after the
        // downstream closes — delivering there would violate that guard.
        // Only real failures need the fallback. The vec is the CANONICAL
        // pending set: the inner fn drains collected rename warnings into it
        // right after collection and only moves everything into the payload
        // at the final render, so whatever failed leaves the full set here.
        deliver_warnings_stderr(&cache_warnings);
    }
    result
}

async fn run_status_scan_locked_inner(
    args: &StatusArgs,
    output: &OutputConfig,
    index_path: &std::path::Path,
    extras: StatusConfigExtras,
    cache_warnings: &mut Vec<StatusWarning>,
) -> CliResult<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::dirty::{DirtyCache, current_index_fingerprint};

    let fingerprint_before =
        current_index_fingerprint(index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_before = Head::current_commit().await.map(|oid| oid.to_string());
    let scan_started_at = crate::internal::dirty::now_timestamp();

    // The io_blocked-aware collection runs FIRST: the legacy raw walker
    // below fails fast on an unreadable directory, which would bypass the
    // partial contract (JSON) and the cache guard (both formats).
    let mut data = collect_status_data(
        args,
        extras,
        &InvocationWarningCtx::from_process_preflight(),
    )
    .await?;
    // Fold the collected (rename) warnings into the canonical pending vec so
    // ANY later failure — recheck, txn, JSON emit, render, summary — reaches
    // the wrapper fallback with the complete set.
    cache_warnings.append(&mut data.warnings);

    let fingerprint_after =
        current_index_fingerprint(index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_after = Head::current_commit().await.map(|oid| oid.to_string());
    if fingerprint_before != fingerprint_after || head_before != head_after {
        return Err(CliError::failure(
            "the index or HEAD changed while scanning; the dirty cache was left untouched",
        )
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("re-run 'libra status --scan' once the concurrent operation finishes"));
    }

    // The CACHE is a whole-repository snapshot, so it is collected WITHOUT
    // the pathspec — still through the bounded, io_blocked-aware
    // accumulator, never a legacy fail-fast walk. It runs BEFORE the guard
    // below so a block discovered only by this pass takes the same shared
    // route: JSON reports the partial result, text fails closed. Returning
    // fatal from inside the collection would bypass `data.io_blocked[]`,
    // the warnings and the completeness flags in JSON mode.
    let snapshot_data = {
        let mut unfiltered = args.clone();
        unfiltered.pathspec.clear();
        // The cache records individual PATHS, so the snapshot walk must not
        // collapse untracked directories into a `dir/` marker the way the
        // display default does — `--cached docs` has to be able to answer
        // with `docs/readme.md`.
        unfiltered.untracked_files = Some(UntrackedFiles::All);
        // Rename detection is DISABLED for the snapshot. Pairing removes the
        // endpoints from `deleted`/`new` and records them under `renamed`,
        // which the cache has no row kind for — so a scan of a renamed file
        // would persist an empty snapshot and a later `--cached --exit-code`
        // would call the repository clean.
        let mut snapshot_extras = extras;
        snapshot_extras.rename_threshold = None;
        collect_status_data(
            &unfiltered,
            snapshot_extras,
            &InvocationWarningCtx::from_process_preflight(),
        )
        .await?
    };
    for event in &snapshot_data.io_blocked {
        if !data
            .io_blocked
            .iter()
            .any(|existing| existing.path == event.path)
        {
            data.io_blocked.push(event.clone());
            data.base_scan_blocked = true;
            let (reason, code) = io_blocked_reason_and_code(event.reason);
            cache_warnings.push(StatusWarning {
                code,
                message: format!(
                    "cannot inspect '{}': {reason}",
                    quote_pathname(&event.path, data.quote_path)
                ),
                source: code.source(),
            });
        }
    }
    data.io_blocked
        .sort_by_key(|event| raw_path_sort_key(&event.path));

    // §B.3.3 dirty-cache guard: an I/O-blocked scan must not write ANY
    // cache content — an unreadable path is "cannot tell", and caching the
    // partial snapshot would prune NEW rows or confirm DELETED ones that
    // were merely unreadable. Text formats fail closed below anyway; JSON
    // reports the partial result while the previous snapshot stays intact.
    if !data.io_blocked.is_empty() {
        if output.is_json() {
            // JSON keeps the partial contract: report the blocked paths and
            // the untouched cache instead of failing, then let the shared
            // arbitration decide the exit code (warning 9 ≻ dirty 1).
            data.warnings = cache_warnings.clone();
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] = serde_json::json!("scan");
            json_data["cache_written"] = serde_json::json!(false);
            // A non-EPIPE stdout failure must not swallow the collected
            // warnings; the OUTER `run_status_scan_locked` wrapper owns the
            // stderr fallback for every non-silent inner failure (a second
            // delivery here double-printed the shared `cache_warnings` set,
            // Codex W5-09 r9 P2).
            emit_json_data("status", &json_data, output)?;
            StatusOutcome::new(&data, args).resolve(output)?;
            return Ok(());
        }
        return Err(CliError::fatal(format!(
            "cannot rebuild the dirty cache: {} path(s) could not be inspected (first: '{}')",
            data.io_blocked.len(),
            quote_pathname(&data.io_blocked[0].path, data.quote_path)
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
        .with_hint("fix the unreadable path permissions and re-run 'libra status --scan'")
        .with_hint(
            "the previous dirty-cache snapshot was left untouched; use --json to inspect \
             the partial result with data.io_blocked[]",
        ));
    }
    // Fully inspectable: the cache rows come from the accumulator walk above.
    let rooted = snapshot_data.to_repo_relative();
    let (staged_raw, unstaged_raw) = (rooted.staged.clone(), rooted.unstaged.clone());
    let SnapshotRows { rows, unencodable } = snapshot_rows(&staged_raw, &unstaged_raw);
    if unencodable > 0 {
        cache_warnings.push(cache_warning(
            StatusWarningCode::DirtyCachePathUnencodable,
            format!(
                "{unencodable} path(s) with non-UTF-8 names were omitted from the dirty cache; \
                 the full status still reports them"
            ),
        ));
    }
    let row_count = rows.len();
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;
    let txn = db
        .begin()
        .await
        .map_err(|e| dirty_cache_error("open a transaction for", anyhow::anyhow!(e)))?;
    DirtyCache::replace_all_with_conn(
        &txn,
        &rows,
        &fingerprint_before,
        head_before.as_deref(),
        &scan_started_at,
    )
    .await
    .map_err(|e| dirty_cache_error("write", e))?;
    txn.commit()
        .await
        .map_err(|e| dirty_cache_error("commit", anyhow::anyhow!(e)))?;

    // NON-DESTRUCTIVE copy into the render payload: the canonical vec stays
    // intact so the wrapper's fallback still holds the complete set if the
    // JSON emit / body render / summary write below fails non-silently.
    // (Success delivers via the payload; the wrapper only fires on Err.)
    data.warnings = cache_warnings.clone();

    if output.is_json() {
        let mut json_data = build_status_json(&data, args);
        json_data["mode"] = serde_json::json!("scan");
        json_data["cached_paths"] = serde_json::json!(row_count);
        // A non-EPIPE stdout failure must not swallow the collected
        // warnings; the OUTER `run_status_scan_locked` wrapper owns the
        // stderr fallback for every non-silent inner failure (a second
        // delivery here double-printed the shared `cache_warnings` set,
        // Codex W5-09 r9 P2).
        emit_json_data("status", &json_data, output)?;
    } else {
        // Deliver AFTER the body renders: a render failure then reaches the
        // wrapper's snapshot fallback instead of double-printing (warnings
        // still fire under --quiet, which only skips the body).
        if !output.quiet {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            render_status_to_writer(&data, args, output, &mut stdout).await?;
            // Fallible write (a bare println! would panic on stdout failure
            // and bypass the wrapper's warning fallback).
            writeln!(stdout, "dirty cache rebuilt ({row_count} paths)").map_err(|error| {
                crate::utils::output::stdout_write_error("write the status scan summary", error)
            })?;
        }
        deliver_warnings_stderr(&data.warnings);
    }
    StatusOutcome::new(&data, args).resolve(output)?;
    Ok(())
}

/// Classify a manual (`kind='unknown'`) mark against the index, bounded and
/// panic-free (deliberately no `Index::is_modified`, which panics on missing
/// entries/files): returns the effective kind, or `None` when clean.
/// §B.3.3 revalidation tri-state: `NotFound` is the only proof of absence —
/// any other metadata error means "cannot tell" and must neither prune a
/// cached NEW row nor confirm a cached DELETED row.
#[derive(Clone, Copy)]
enum CachedPathState {
    Exists,
    Gone,
    /// Carries the §B.6.0.1 reason so a blocked re-verification can be
    /// reported through `data.io_blocked[]` with the same taxonomy the
    /// worktree walk uses, instead of a generic "something failed".
    Blocked(crate::command::status_probe::IoBlockedReason),
}

/// Classify a revalidation I/O error into the §B.6.0.1 reason taxonomy.
/// `TimedOut` is its OWN reason: the docs promise consumers can tell a hung
/// mount from an ordinary read failure, and collapsing it into `io_error`
/// silently breaks that distinction on the cache path.
fn blocked_reason(error: &io::Error) -> crate::command::status_probe::IoBlockedReason {
    use crate::command::status_probe::IoBlockedReason;
    match error.kind() {
        io::ErrorKind::PermissionDenied => IoBlockedReason::PermissionDenied,
        io::ErrorKind::TimedOut => IoBlockedReason::IoTimeout,
        _ => IoBlockedReason::IoError,
    }
}

/// Hash a worktree file for cache revalidation under the §B.3.3 deadline.
/// A reclaimed read surfaces as `TimedOut` so callers classify it as blocked
/// rather than as a content change.
fn hash_under_deadline(abs: &Path, workdir: &Path) -> io::Result<git_internal::hash::ObjectHash> {
    match crate::command::status_io_worker::deadline_file_blob_hash(abs, workdir) {
        Ok(result) => result,
        Err(()) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "content read exceeded the status I/O deadline",
        )),
    }
}

fn cached_path_state(abs: &Path) -> CachedPathState {
    use crate::command::status_probe::IoBlockedReason;
    // §B.3.3: revalidation stats run under the same deadline as the full
    // scan. A cached path that became a FIFO or moved onto a hung mount must
    // reclaim the caller and keep its cache row, not block `--check-dirty`
    // forever.
    let stat = match crate::command::status_io_worker::deadline_stat(abs) {
        Ok(result) => result,
        Err(()) => return CachedPathState::Blocked(IoBlockedReason::IoTimeout),
    };
    match stat {
        Ok(_) => CachedPathState::Exists,
        Err(error) if error.kind() == io::ErrorKind::NotFound => CachedPathState::Gone,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            CachedPathState::Blocked(IoBlockedReason::PermissionDenied)
        }
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            CachedPathState::Blocked(IoBlockedReason::IoTimeout)
        }
        Err(_) => CachedPathState::Blocked(IoBlockedReason::IoError),
    }
}

/// Outcome of re-classifying a manual `libra dirty` mark (stored as
/// `unknown`). The `Blocked` arm exists because "cannot inspect" is neither
/// dirty nor clean: collapsing it into either one lets `--check-dirty`
/// delete a still-valid row or render a present file as deleted.
enum ManualMarkClass {
    Dirty(&'static str),
    Clean,
    Blocked(crate::command::status_probe::IoBlockedReason),
}

fn classify_manual_mark(index: &Index, workdir: &std::path::Path, stored: &str) -> ManualMarkClass {
    use crate::internal::dirty;
    let native = dirty::stored_path_to_native(stored);
    let Some(path_str) = native.to_str() else {
        return ManualMarkClass::Dirty(dirty::KIND_NEW); // undecodable: over-report
    };
    let tracked = index.tracked(path_str, 0);
    let abs = workdir.join(&native);
    // Tri-state stat (§B.6.0.1): EACCES must not masquerade as absence, or a
    // tracked-but-unreadable path is reported deleted and a manual mark on
    // an unreadable untracked path is pruned from the cache.
    let exists = match cached_path_state(&abs) {
        CachedPathState::Exists => true,
        CachedPathState::Gone => false,
        CachedPathState::Blocked(reason) => return ManualMarkClass::Blocked(reason),
    };
    match (tracked, exists) {
        (false, true) => ManualMarkClass::Dirty(dirty::KIND_NEW),
        (false, false) => ManualMarkClass::Clean, // neither tracked nor present: not dirty
        (true, false) => ManualMarkClass::Dirty(dirty::KIND_DELETED),
        (true, true) => {
            // Content confirm (no stat shortcut: manual marks are few, and a
            // wrong stat shortcut here would silently drop a real edit).
            match hash_under_deadline(&abs, workdir) {
                Ok(hash) if index.verify_hash(path_str, 0, &hash) => ManualMarkClass::Clean,
                Ok(_) => ManualMarkClass::Dirty(dirty::KIND_MODIFIED),
                // Readable metadata but an unreadable body: still "cannot
                // inspect", so never let it confirm or prune a cache row.
                Err(error) => ManualMarkClass::Blocked(blocked_reason(&error)),
            }
        }
    }
}

/// `status --cached` and `status --check-dirty`: consume / re-verify the
/// cache. Any freshness doubt degrades to the full reconcile (the cache may
/// over-report or degrade, never silently under-report).
pub(super) async fn run_status_cache_mode(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    output: &OutputConfig,
) -> CliResult<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::dirty::{self, CacheState, DirtyCache, current_index_fingerprint};

    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let fingerprint =
        current_index_fingerprint(&index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_oid = Head::current_commit().await.map(|oid| oid.to_string());
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;
    let meta = DirtyCache::meta_with_conn(&db)
        .await
        .map_err(|e| dirty_cache_error("read", e))?;
    let state = DirtyCache::classify(meta.as_ref(), &fingerprint, head_oid.as_deref());

    if state != CacheState::Fresh {
        // Degrade to the full reconcile — never trust a doubtful cache.
        let mut data = collect_status_data(
            args,
            extras,
            &InvocationWarningCtx::from_process_preflight(),
        )
        .await?;
        data.warnings.push(cache_warning(
            StatusWarningCode::DirtyCacheStaleFallback,
            format!(
                "dirty cache is {}; falling back to the full status (run 'libra status --scan' to rebuild)",
                state.as_str()
            ),
        ));
        if output.is_json() {
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] =
                serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
            json_data["freshness"] = serde_json::json!("full");
            json_data["cache_state"] = serde_json::json!(state.as_str());
            if let Err(error) = emit_json_data("status", &json_data, output) {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
                return Err(error);
            }
        } else {
            // Fail closed on any blocked path BEFORE rendering — and before
            // the quiet check. The fallback runs a full scan, so it can
            // discover blocked paths exactly like the normal path; skipping
            // the guard here would let `--cached` print a partial body (or
            // exit 0 under `--quiet`) on a repository it could not inspect.
            fail_closed_on_io_blocked(&data, output).inspect_err(|error| {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
            })?;
            // Deliver AFTER the body: EPIPE mid-render stays fully silent
            // (P0-06), while a real render failure still surfaces the
            // warning before propagating.
            if !output.quiet {
                let mut stdout = std::io::stdout();
                if let Err(error) = render_status_to_writer(&data, args, output, &mut stdout).await
                {
                    if !error.is_silent() {
                        deliver_warnings_stderr(&data.warnings);
                    }
                    return Err(error);
                }
            }
            deliver_warnings_stderr(&data.warnings);
        }
        StatusOutcome::new(&data, args).resolve(output)?;
        return Ok(());
    }

    let rows = DirtyCache::list_with_conn(&db)
        .await
        .map_err(|e| dirty_cache_error("read", e))?;
    let workdir = util::try_working_dir()
        .map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let index = load_status_index()?;

    // Build the raw sets from the cache (staged snapshot + unstaged rows +
    // classified manual marks), optionally re-verifying (--check-dirty).
    let mut staged = Changes::default();
    let mut unstaged = Changes::default();
    let mut pruned: Vec<(String, String)> = Vec::new();
    let mut confirmed: Vec<(String, String)> = Vec::new();
    // Rows whose re-verification could not run (§B.6.0.1). These are neither
    // confirmed nor pruned, and their presence suppresses the cache write
    // entirely — a partial re-verification must never be persisted as if it
    // had inspected everything.
    let mut blocked_paths: Vec<(PathBuf, crate::command::status_probe::IoBlockedReason)> =
        Vec::new();
    for row in &rows {
        let native = dirty::stored_path_to_native(&row.path);
        let verify = args.check_dirty;
        match row.kind.as_str() {
            dirty::KIND_STAGED_NEW => staged.new.push(native),
            dirty::KIND_STAGED_MODIFIED => staged.modified.push(native),
            dirty::KIND_STAGED_DELETED => staged.deleted.push(native),
            dirty::KIND_NEW => {
                // An undecodable stored path cannot be re-verified — keep it
                // (the cache must never under-report a recorded fact).
                let Some(path_str) = native.to_str() else {
                    unstaged.new.push(native);
                    continue;
                };
                let abs = workdir.join(&native);
                // ONE tri-state stat, reused for both the guard and the
                // decision. Calling it twice reintroduces the race it exists
                // to close: a permission change between the two calls makes
                // the second read "gone" while `blocked_paths` stays empty,
                // and the row is pruned from a cache it was never verified
                // against.
                let state = if verify {
                    cached_path_state(&abs)
                } else {
                    CachedPathState::Exists
                };
                if let (true, CachedPathState::Blocked(reason)) = (verify, state) {
                    // Unreadable is NOT proof the path went away (§B.6.0.1):
                    // keep the cached fact, write nothing — pruning here
                    // would silently forget a real untracked file.
                    blocked_paths.push((native.clone(), reason));
                    unstaged.new.push(native);
                    continue;
                }
                let still = !verify
                    || (matches!(state, CachedPathState::Exists) && !index.tracked(path_str, 0));
                if still {
                    unstaged.new.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            dirty::KIND_MODIFIED => {
                let Some(path_str) = native.to_str() else {
                    unstaged.modified.push(native);
                    continue;
                };
                let abs = workdir.join(&native);
                // Single tri-state stat, reused below (see the NEW branch).
                let state = if verify {
                    cached_path_state(&abs)
                } else {
                    CachedPathState::Exists
                };
                if let (true, CachedPathState::Blocked(reason)) = (verify, state) {
                    // Unreadable: keep the cached fact, write nothing.
                    blocked_paths.push((native.clone(), reason));
                    unstaged.modified.push(native);
                    continue;
                }
                // The stat can succeed while the CONTENT read fails (a
                // chmod-000 file still stats fine), so the hash failure is
                // its own blocked case: keep the row, report it, write
                // nothing. Treating it as "still modified" without an event
                // would leave `io_blocked[]` and `warnings[]` empty on a run
                // that demonstrably could not inspect the file.
                let mut still = !verify;
                if verify {
                    still = index.tracked(path_str, 0)
                        && matches!(state, CachedPathState::Exists)
                        && match hash_under_deadline(&abs, &workdir) {
                            Ok(hash) => !index.verify_hash(path_str, 0, &hash),
                            Err(error) => {
                                blocked_paths.push((native.clone(), blocked_reason(&error)));
                                true // unreadable: keep (over-report)
                            }
                        };
                }
                if still {
                    unstaged.modified.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            dirty::KIND_DELETED => {
                let Some(path_str) = native.to_str() else {
                    unstaged.deleted.push(native);
                    continue;
                };
                // `--cached` promises NO worktree walk: only `--check-dirty`
                // re-verifies, so the stat is skipped entirely rather than
                // performed and discarded (which would still take an I/O
                // worker slot and could wait out a deadline on a hung mount).
                let state = if verify {
                    cached_path_state(&workdir.join(&native))
                } else {
                    CachedPathState::Exists
                };
                if let (true, CachedPathState::Blocked(reason)) = (verify, state) {
                    // Unreadable is NOT proof of deletion (§B.6.0.1): keep
                    // the cached fact, write nothing — never confirm.
                    blocked_paths.push((native.clone(), reason));
                    unstaged.deleted.push(native);
                    continue;
                }
                let still = !verify
                    || (index.tracked(path_str, 0) && matches!(state, CachedPathState::Gone));
                if still {
                    unstaged.deleted.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            _ => {
                // Manual 'unknown' marks: classified in memory, always content
                // confirmed (both modes — cheap, marks are few).
                if !verify {
                    // `--cached` consumes the snapshot only (documented "no
                    // worktree walk"). A manual mark has no recorded kind, so
                    // the conservative reading is "still dirty" — never a
                    // stat/hash that could block on the worktree.
                    unstaged.modified.push(native);
                    continue;
                }
                match classify_manual_mark(&index, &workdir, &row.path) {
                    ManualMarkClass::Dirty(dirty::KIND_NEW) => unstaged.new.push(native),
                    ManualMarkClass::Dirty(dirty::KIND_DELETED) => unstaged.deleted.push(native),
                    ManualMarkClass::Dirty(_) => unstaged.modified.push(native),
                    ManualMarkClass::Blocked(reason) => {
                        // Cannot inspect: keep the mark and surface it as a
                        // blocked path, never prune and never invent a
                        // deletion. Text formats fail closed downstream;
                        // `--json` reports it in `data.io_blocked[]`.
                        blocked_paths.push((native.clone(), reason));
                        unstaged.modified.push(native);
                    }
                    ManualMarkClass::Clean => {
                        if verify {
                            pruned.push((row.path.clone(), row.kind.clone()));
                        }
                        // --cached: clean manual marks are dropped from the
                        // VIEW but kept in the cache (read-only fast path).
                    }
                }
            }
        }
    }
    let checked = rows.len();
    // Test-only fault-injection seam (debug builds only, runtime-gated on
    // LIBRA_TEST like the rest of the seam family): widen the read→re-verify
    // window so the mid-read concurrent-invalidate branch can be triggered
    // deterministically. Compiled out of release binaries.
    #[cfg(debug_assertions)]
    if std::env::var_os("LIBRA_TEST").is_some_and(|v| v == "1")
        && let Some(ms) = std::env::var("LIBRA_TEST_CACHE_READ_PAUSE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
    {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    // Re-verify the epoch AFTER processing: a concurrent index/HEAD change
    // since the initial classify would make this view (and any prune) stale —
    // degrade instead of committing or rendering it as fresh.
    let fingerprint_now =
        current_index_fingerprint(&index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_now = Head::current_commit().await.map(|oid| oid.to_string());
    if fingerprint_now != fingerprint || head_now != head_oid {
        let mut data = collect_status_data(
            args,
            extras,
            &InvocationWarningCtx::from_process_preflight(),
        )
        .await?;
        data.warnings.push(cache_warning(
            StatusWarningCode::DirtyCacheConcurrentInvalidate,
            "the index or HEAD changed while reading the dirty cache; falling back to the full status",
        ));
        if output.is_json() {
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] =
                serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
            json_data["freshness"] = serde_json::json!("full");
            json_data["cache_state"] = serde_json::json!("stale");
            if let Err(error) = emit_json_data("status", &json_data, output) {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
                return Err(error);
            }
        } else {
            // Same fail-closed guard as every other text path (§B.6.0.1):
            // the concurrent-invalidate fallback also runs a full scan, so
            // it can discover blocked paths and must not print a partial
            // body or exit 0 under `--quiet`.
            fail_closed_on_io_blocked(&data, output).inspect_err(|error| {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
            })?;
            // Deliver AFTER the body: EPIPE mid-render stays fully silent
            // (P0-06), while a real render failure still surfaces the
            // warning before propagating.
            if !output.quiet {
                let mut stdout = std::io::stdout();
                if let Err(error) = render_status_to_writer(&data, args, output, &mut stdout).await
                {
                    if !error.is_silent() {
                        deliver_warnings_stderr(&data.warnings);
                    }
                    return Err(error);
                }
            }
            deliver_warnings_stderr(&data.warnings);
        }
        StatusOutcome::new(&data, args).resolve(output)?;
        return Ok(());
    }
    // §B.6.0.1: a re-verification that could not inspect every row must not
    // persist its partial view — pruning on an incomplete pass is how a
    // still-dirty path silently disappears from the cache.
    if args.check_dirty && !blocked_paths.is_empty() {
        pruned.clear();
        confirmed.clear();
    }
    if args.check_dirty && (!pruned.is_empty() || !confirmed.is_empty()) {
        let txn = db
            .begin()
            .await
            .map_err(|e| dirty_cache_error("open a transaction for", anyhow::anyhow!(e)))?;
        DirtyCache::prune_and_confirm_with_conn(&txn, &pruned, &confirmed)
            .await
            .map_err(|e| dirty_cache_error("update", e))?;
        txn.commit()
            .await
            .map_err(|e| dirty_cache_error("commit", anyhow::anyhow!(e)))?;
    }

    // Assemble display data: cheap fresh pieces (head/upstream/merge state),
    // cache-derived changes (cwd-relative for display), NO rename detection
    // (would need object loads; documented) and no worktree walk.
    // Result-returning, like the full-scan path: a corrupt HEAD row (or an
    // OID of the wrong hash algorithm) must surface as an actionable error,
    // not as a process panic from the lossy `Head::current()` wrapper.
    let head = Head::current_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD", error))?;
    let head_oid_hash = Head::current_commit_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD commit", error))?;
    let staged = staged.to_relative();
    let mut unstaged = unstaged.to_relative();
    // Honor the resolved display defaults exactly like the full status
    // (P1-05d): `status.showUntrackedFiles=no`/`-uno` hides untracked
    // entries (the cache stores explicit paths, so `normal` and `all`
    // render identically here), and `--show-stash`/`status.showStash`
    // surfaces the stash count. `status.relativePaths=false` is applied by
    // the shared renderer.
    if args.untracked_files == Some(UntrackedFiles::No) {
        unstaged.new.clear();
    }
    let stash_count = if args.show_stash {
        Some(stash::get_stash_num().map_err(|detail| {
            CliError::fatal(format!("failed to read stash state for status: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("repair or remove the corrupt stash log, then retry")
        })?)
    } else {
        None
    };
    let mut upstream_warnings = Vec::new();
    let upstream =
        resolve_upstream_info(&head, head_oid_hash.as_ref(), &mut upstream_warnings).await?;
    let merge_state = match merge::MergeState::load_optional_sync().map_err(|detail| {
        CliError::fatal(format!("failed to inspect merge state: {detail}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        Some(state) => {
            let conflicted_paths =
                merge::unresolved_conflicted_paths(&index, &state.conflicted_paths);
            Some(MergeStatusInfo {
                target_ref: state.target_ref.clone(),
                unresolved_count: conflicted_paths.len(),
                conflicted_paths,
            })
        }
        None => None,
    };
    let mut data = StatusData {
        head,
        has_commits: head_oid_hash.is_some(),
        head_oid: head_oid_hash,
        staged,
        unstaged,
        unmerged: vec![],
        ignored_files: vec![],
        stash_count,
        upstream,
        merge_state,
        sequence_notice: sequence_notice().await?,
        sparse_view_active: crate::internal::sparse::SparseView::load(
            &crate::internal::worktree_scope::WorktreeScope::for_request(),
        )
        .await
        .is_active(),
        porcelain_v2: None,
        staged_rename_details: RenameDetails::new(),
        unstaged_rename_details: RenameDetails::new(),
        // One `worktree_*` warning per blocked row, exactly like the full
        // scan: `io_blocked[]` and `warnings[]` are a documented 1:1 pairing,
        // and `--exit-code-on-warning` reads the warning list, so an empty
        // one here would silently downgrade exit 9 to exit 1.
        warnings: {
            // Cache mode is a CLI-only path, so its invocation context is
            // the process preflight buffer — bound once here rather than read
            // at each use.
            let cache_warning_ctx = InvocationWarningCtx::from_process_preflight();
            let mut preflight: Vec<StatusWarning> = cache_warning_ctx
                .preflight_messages()
                .iter()
                .map(|message| StatusWarning {
                    code: StatusWarningCode::RepositoryPreflight,
                    message: message.clone(),
                    source: StatusWarningCode::RepositoryPreflight.source(),
                })
                .collect();
            let mut seen: HashSet<PathBuf> = HashSet::new();
            let mut sorted = blocked_paths.clone();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            sorted
                .into_iter()
                .filter(|(path, _)| seen.insert(path.clone()))
                .map(|(path, reason)| {
                    let (text, code) = io_blocked_reason_and_code(reason);
                    StatusWarning {
                        code,
                        message: format!(
                            "cannot inspect '{}': {text}",
                            quote_pathname(&path, extras.quote_path)
                        ),
                        source: code.source(),
                    }
                })
                .collect::<Vec<_>>()
                .into_iter()
                .chain(preflight.drain(..))
                .chain(upstream_warnings)
                .collect()
        },
        quote_path: extras.quote_path,
        io_blocked: {
            let mut events: Vec<_> = blocked_paths
                .iter()
                .map(
                    |(path, reason)| crate::command::status_probe::IoBlockedEvent {
                        path: path.clone(),
                        reason: *reason,
                        absorbed: false,
                    },
                )
                .collect();
            events.sort_by_key(|event| raw_path_sort_key(&event.path));
            events.dedup_by(|a, b| a.path == b.path);
            events
        },
        // A re-verification that could not inspect every row is NOT a
        // complete scan: reporting `base_scan_complete: true` alongside a
        // non-empty `io_blocked[]` tells automation the cached answer is
        // authoritative when it demonstrably is not.
        base_scan_blocked: !blocked_paths.is_empty(),
        rename_scan_blocked: false,
    };
    filter_status_data_by_pathspec(&mut data, args)?;

    if output.is_json() {
        let mut json_data = build_status_json(&data, args);
        json_data["mode"] = serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
        json_data["freshness"] = serde_json::json!("cached");
        json_data["cache_state"] = serde_json::json!("fresh");
        json_data["cached_paths"] = serde_json::json!(checked);
        if args.check_dirty {
            json_data["checked_paths"] = serde_json::json!(checked);
            json_data["stale_paths"] = serde_json::json!(
                pruned
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>()
            );
        }
        // A non-EPIPE stdout failure must not swallow the collected
        // warnings: JSON is their only channel, so they fall back to stderr
        // rather than vanishing with the envelope.
        if let Err(error) = emit_json_data("status", &json_data, output) {
            if !error.is_silent() {
                deliver_warnings_stderr(&data.warnings);
            }
            return Err(error);
        }
    } else {
        deliver_warnings_stderr(&data.warnings);
        fail_closed_on_io_blocked(&data, output)?;
        if !output.quiet {
            render_cached_status_body(&data, args, output, checked, pruned.len()).await?;
        }
    }
    StatusOutcome::new(&data, args).resolve(output)?;
    Ok(())
}

async fn render_cached_status_body(
    data: &StatusData,
    args: &StatusArgs,
    output: &OutputConfig,
    checked: usize,
    pruned: usize,
) -> CliResult<()> {
    {
        use std::io::Write;
        let mut stdout = std::io::stdout();
        render_status_to_writer(data, args, output, &mut stdout).await?;
        if args.check_dirty {
            // Fallible write: a bare println! would panic on stdout EPIPE.
            writeln!(
                stdout,
                "dirty cache re-verified ({checked} checked, {pruned} pruned)"
            )
            .map_err(|error| {
                crate::utils::output::stdout_write_error("write the check-dirty summary", error)
            })?;
        }
    }
    Ok(())
}
