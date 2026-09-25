//! Status collection and bounded rename orchestration.

use super::*;

/// Human advisory for a non-merge sequence in progress (read-only detection).
pub(super) async fn sequence_notice() -> CliResult<Option<String>> {
    use crate::internal::sequencer::{self, ActiveSequenceKind, SequenceKind};
    let active = sequencer::detect_active_operation()
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to inspect in-progress operation state: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("repair the repository sequencer state, then retry 'libra status'")
        })?;
    Ok(match active {
        Some(ActiveSequenceKind::Am) => Some(
            "You are in the middle of an am operation; use 'libra am --continue', '--skip', or '--abort'."
                .to_string(),
        ),
        Some(ActiveSequenceKind::Known(SequenceKind::CherryPick)) => Some(
            "cherry-pick in progress; run 'libra cherry-pick --continue' or '--abort'".to_string(),
        ),
        Some(ActiveSequenceKind::Known(SequenceKind::Revert)) => {
            Some("revert in progress; run 'libra revert --continue' or '--abort'".to_string())
        }
        Some(ActiveSequenceKind::Known(SequenceKind::Rebase)) => {
            Some("rebase in progress; run 'libra rebase --continue' or '--abort'".to_string())
        }
        // An ambiguous legacy directory is REPORTED, not fatal: this worktree
        // has no sequencer state, so status is exactly the command that should
        // still work and tell the user what is there. (A sequence-START path
        // refuses it — see `ensure_none_for`.)
        // `bisect` has its own dedicated status rendering elsewhere; the
        // sequence advisory does not duplicate it.
        Some(ActiveSequenceKind::Bisect) => None,
        Some(ActiveSequenceKind::AmbiguousLegacy(state)) => {
            // Medium-accurate guidance: a table is not a file, and telling a
            // user to delete a directory that does not exist is advice they
            // cannot act on.
            let (what, how) = state.describe();
            Some(format!(
                "{what}, and this repository has linked-worktree history. {how}."
            ))
        }
        // Merge has its own dedicated rendering below.
        Some(ActiveSequenceKind::Known(SequenceKind::Merge)) | None => None,
    })
}

impl StatusData {
    pub(super) fn is_dirty(&self) -> bool {
        !self.staged.is_empty()
            || !self.unstaged.is_empty()
            || self.merge_state.is_some()
            || !self.unmerged.is_empty()
            // §B.6.0.1: "cannot inspect" must never report clean.
            || !self.io_blocked.is_empty()
    }
}

/// Collect all status data in one pass, eliminating duplicate computation
/// between human/JSON/short/porcelain renderers.
pub(super) async fn collect_status_data(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    warning_ctx: &InvocationWarningCtx,
) -> CliResult<StatusData> {
    // lore.md 2.4: layer-overlay paths are excluded from status like ignored
    // files (a no-op with no layers). W1 §C.4.1.1: refreshed with this
    // request's resolved worktree scope.
    crate::internal::layer::refresh_exclusion_snapshot(
        &crate::internal::worktree_scope::WorktreeScope::for_request(),
    )
    .await;
    if is_bare_repository().await? {
        return Err(CliError::fatal("this operation must be run in a work tree")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("this command requires a working tree; bare repositories do not have one"));
    }
    let _status_io_root = crate::command::status_io_worker::begin_status_io_root_session()
        .map_err(|source| {
            CliError::fatal(format!("cannot resolve worktree for status I/O: {source}"))
        })?;
    let ignore_case = effective_ignore_case_for_status().await?;

    let head = Head::current_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD", error))?;
    let head_oid = Head::current_commit_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD commit", error))?;
    let has_commits = head_oid.is_some();

    let mut staged = changes_to_be_committed_safe()
        .await
        .map(|c| c.to_relative())
        .map_err(CliError::from)?;
    // ADR-FM-05 (FM-04): mode-only worktree changes are reported only when
    // core.fileMode is enabled.
    let file_mode = crate::internal::config::core_file_mode().await?;
    let worktree = status_untracked::collect_status_worktree_changes(
        args.untracked_files.unwrap_or(UntrackedFiles::Normal),
        args.ignored,
        ignore_case,
        file_mode,
    )
    .map_err(CliError::from)?;
    let mut unstaged = status_untracked::changes_to_current_directory(worktree.unstaged);
    let unmerged = unmerged::collect(&worktree.index)
        .into_iter()
        .map(|entry| {
            let current_path = util::workdir_to_current(&entry.path);
            entry.with_path(current_path)
        })
        .collect::<Vec<_>>();
    let unmerged_paths = unmerged
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<HashSet<_>>();
    unstaged.new.retain(|path| !unmerged_paths.contains(path));
    let ignored_files = worktree
        .ignored_files
        .into_iter()
        .map(|path| {
            // The marker-preserving projection (directory markers are
            // built on the raw name; see `with_dir_marker`).
            let projected = util::workdir_to_current(&path);
            with_dir_marker(&path, projected)
        })
        .collect();
    let mut io_blocked = worktree.io_blocked;
    let base_scan_blocked = !io_blocked.is_empty();
    // Tracked separately from `base_scan_blocked`: a block during the
    // BASE scan does not mean rename detection degraded, and reporting
    // both flags false for one base-scan EACCES tells consumers the
    // rename pairing is unreliable when it was never attempted.
    let mut rename_scan_blocked = false;
    // One accumulator for BOTH detection sides: §B.5 requires exactly one
    // warning per {code, source} for the whole run, so the per-side stats
    // are folded together and rendered once (see `merge_rename_stats`).
    let mut rename_stats = rename_detect::RenameDetectStats::default();
    let mut rename_budgets = RenameBudgets::new();
    let mut maybe_index = Some(worktree.index);

    // Resolve rename detection (§B.5). Precedence: CLI flags always win —
    // `--no-renames` disables, `--find-renames[=N]`/`--renames` enable at the
    // given (or default 50%) threshold. Otherwise the resolved
    // `status.renames`/`diff.renames` config applies (`false` disables). When
    // nothing is set, rename detection is ON at 50%, matching Git.
    // `--cached`/`--check-dirty` (Libra dirty-cache extensions) never run it.
    let rename_threshold: Option<u32> = if args.cached || args.check_dirty {
        None
    } else {
        extras.rename_threshold
    };

    // Apply rename detection before collapsing untracked dirs / porcelain
    // metadata. Staged snapshot: old = HEAD tree, new = index stage-0.
    // Unstaged snapshot: old = index stage-0, new = worktree — but untracked
    // paths only become destinations under the `status.renameUntracked`
    // extension (§B.3.1; Git default: a tracked→untracked move is `D` + `??`).
    let mut staged_rename_details: RenameDetails = HashMap::new();
    let mut unstaged_rename_details: RenameDetails = HashMap::new();
    let mut warnings: Vec<StatusWarning> = Vec::new();
    if let Some(threshold) = rename_threshold {
        let head_blobs = head_oid
            .as_ref()
            .map(load_head_tree_blobs)
            .transpose()?
            .unwrap_or_default();
        let index_blobs = maybe_index
            .as_ref()
            .map(load_index_stage0_blobs)
            .unwrap_or_default();
        // Git 0..=60000 similarity scale (already engine-scale here).
        let config = rename_detect::RenameDetectConfig {
            threshold,
            rename_limit: extras.rename_limit,
            comparison_budget: Some(status_comparison_budget()),
        };
        // An unresolved conflict is NOT a staged deletion for rename
        // pairing: the stage-0-less index classifies it as deleted, and a
        // same-content staged addition could otherwise consume it as a
        // rename SOURCE — emitting a `2` record for a path whose only
        // truthful spelling is the unmerged `u` row. Pull conflicts out
        // for the detection pass only and restore them after, leaving
        // every format's classification of the conflict itself untouched
        // (2026-08-06 R0-5 review).
        let conflicted_staged_deletes: Vec<PathBuf> = staged
            .deleted
            .iter()
            .filter(|path| unmerged_paths.contains(*path))
            .cloned()
            .collect();
        if !conflicted_staged_deletes.is_empty() {
            staged.deleted.retain(|path| !unmerged_paths.contains(path));
        }
        detect_renames_in_changes(
            &mut staged,
            &config,
            RenameBlobSide::Known(&head_blobs),
            RenameBlobSide::Known(&index_blobs),
            &mut staged_rename_details,
            &mut rename_stats,
            &mut rename_budgets,
        );
        if !conflicted_staged_deletes.is_empty() {
            staged.deleted.extend(conflicted_staged_deletes);
            staged.deleted.sort();
        }
        // §B.3.1 Git default: unstaged "new" entries are untracked paths,
        // which may only be consumed as rename destinations under the
        // `status.renameUntracked` extension. Skipping detection keeps a
        // tracked→untracked move rendered as `D` + `??`.
        //
        // R0-3: under the extension, DESTINATIONS come from the bounded
        // probe (§B.3.1.1–§B.3.2) — decoupled from the untracked display
        // scan (`-uno` hides markers, never the probe) and qualified by the
        // same tracked/ignore layering. The probe only runs when there is a
        // deleted side to pair against.
        if extras.rename_untracked
            && !unstaged.deleted.is_empty()
            && let Some(index_ref) = maybe_index.as_ref()
        {
            let workdir = util::working_dir();
            let compiled_pathspecs = if args.pathspec.is_empty() {
                None
            } else {
                Some(
                    PathspecSet::from_workdir(&args.pathspec, &util::cur_dir(), &workdir)
                        .map_err(pathspec_error_to_cli)?,
                )
            };
            let tracked_paths = crate::command::status_untracked_paths::TrackedPaths::from_index(
                index_ref,
                ignore_case,
            );
            let filter = crate::command::status_probe::DestinationFilter {
                workdir: &workdir,
                index: index_ref,
                tracked: &tracked_paths,
                pathspecs: compiled_pathspecs.as_ref(),
            };
            let roots =
                crate::command::status_probe::pathspec_probe_roots(compiled_pathspecs.as_ref());
            let outcome = crate::command::status_probe::probe_rename_destinations(
                &roots,
                &filter,
                crate::command::status_probe::ProbeLimits::effective(),
            );
            // §B.3.2 merge rules (R0-8): blocked probe paths accumulate into
            // `data.io_blocked` — text formats fail closed at render time,
            // JSON keeps pairing and reports the partial contract.
            rename_scan_blocked |= !outcome.io_blocked.is_empty();
            io_blocked.extend(outcome.io_blocked.iter().cloned());
            if let Some(kind) = outcome.truncated {
                warnings.push(StatusWarning {
                    code: StatusWarningCode::ProbeTruncated,
                    message: format!(
                        "rename-destination probe truncated: {} budget exhausted; rename detection may be incomplete",
                        match kind {
                            crate::command::status_probe::ProbeBudgetKind::Enumeration => "enumeration",
                            crate::command::status_probe::ProbeBudgetKind::Destination => "destination",
                        }
                    ),
                    source: StatusWarningCode::ProbeTruncated.source(),
                });
            }
            if outcome.encoding_skipped > 0 {
                // §B.6.1 / DEFER-02: non-UTF-8 names keep their base `??`
                // rows but sit out rename scoring until R0.5 — one
                // deduplicated warning covers every skipped candidate.
                warnings.push(StatusWarning {
                    code: StatusWarningCode::RenamePathEncodingUnsupported,
                    message: format!(
                        "rename detection skipped {} candidate(s) with non-UTF-8 names; their untracked/base status is unaffected",
                        outcome.encoding_skipped
                    ),
                    source: StatusWarningCode::RenamePathEncodingUnsupported.source(),
                });
            }
            // Detection runs on the probe's destination set (display base);
            // consumed destinations then collapse their display rows and
            // `? dir/` markers (§B.3.5).
            let destinations_display: Vec<PathBuf> = outcome
                .destinations
                .iter()
                .map(util::workdir_to_current)
                .collect();
            let consumed = detect_renames_with_destinations(
                &mut unstaged,
                &config,
                RenameBlobSide::Known(&index_blobs),
                &destinations_display,
                &mut unstaged_rename_details,
                &mut rename_stats,
                &mut rename_budgets,
            );
            let complete_roots: Vec<PathBuf> = outcome
                .complete_roots
                .iter()
                .map(|root| {
                    if root.as_os_str().is_empty() {
                        // "" = the whole worktree; keep it empty so the
                        // collapse treats every marker as governed.
                        PathBuf::new()
                    } else {
                        util::workdir_to_current(root)
                    }
                })
                .collect();
            crate::command::status_probe::collapse_untracked_markers(
                &mut unstaged.new,
                &destinations_display,
                &consumed,
                &complete_roots,
            );
        }
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

    // Resolve upstream tracking info
    let upstream = resolve_upstream_info(&head, head_oid.as_ref(), &mut warnings).await?;
    let merge_state = match merge::MergeState::load_optional_sync().map_err(|detail| {
        CliError::fatal(format!("failed to inspect merge state: {detail}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        Some(state) => {
            if maybe_index.is_none() {
                maybe_index = Some(load_status_index()?);
            }
            let index = maybe_index
                .as_ref()
                .ok_or_else(|| CliError::internal("status index should be loaded"))?;
            let conflicted_paths =
                merge::unresolved_conflicted_paths(index, &state.conflicted_paths);
            Some(MergeStatusInfo {
                target_ref: state.target_ref,
                unresolved_count: conflicted_paths.len(),
                conflicted_paths,
            })
        }
        None => None,
    };
    let porcelain_v2 = if matches!(args.porcelain, Some(PorcelainVersion::V2)) {
        let index = maybe_index
            .take()
            .ok_or_else(|| CliError::internal("porcelain v2 metadata should be loaded"))?;
        Some(std::sync::Arc::new(build_porcelain_v2_data(
            index,
            head_oid.as_ref(),
        )?))
    } else {
        None
    };

    // The engine's own warnings are the rename-side degradation signal. The
    // worktree/metadata CODES are shared with the io_blocked mapping (a
    // base-scan EACCES emits `worktree_permission_denied` too), so the flag
    // is captured HERE — before the io_blocked-derived warnings are
    // synthesized into the same list — rather than inferred from codes.
    warnings_from_rename_stats(&rename_stats, &mut warnings);
    // Preflight advisories join the structured list before anything reads
    // it, so exit arbitration and the JSON payload see the same set.
    for message in warning_ctx.preflight_messages() {
        warnings.push(StatusWarning {
            code: StatusWarningCode::RepositoryPreflight,
            message: message.clone(),
            source: StatusWarningCode::RepositoryPreflight.source(),
        });
    }
    // The upstream-count warning is a `metadata` read failure too, but it
    // never touched rename detection.
    rename_scan_blocked |= warnings.iter().any(|warning| {
        matches!(
            warning.source,
            StatusWarningSource::Worktree | StatusWarningSource::Metadata
        ) && warning.code != StatusWarningCode::UpstreamCountsUnavailable
    });
    let mut data = StatusData {
        head,
        head_oid,
        has_commits,
        staged,
        unstaged,
        unmerged,
        ignored_files,
        stash_count,
        upstream,
        merge_state,
        sequence_notice: sequence_notice().await?,
        sparse_view_active: crate::internal::sparse::SparseView::load(
            &crate::internal::worktree_scope::WorktreeScope::for_request(),
        )
        .await
        .is_active(),
        porcelain_v2,
        staged_rename_details,
        unstaged_rename_details,
        warnings: {
            // §B.5/§B.6.0.1: every blocked path contributes its
            // worktree-family warning HERE (not at JSON-render time), so
            // exit arbitration (`--exit-code-on-warning` → 9) and the
            // stderr delivery of text formats see exactly the same set.
            // The list is then deduplicated on the FULL {code, source,
            // message} triple, not just {code, source}: two detection passes
            // (staged + unstaged) must not double-report the same
            // degradation, but two different blocked paths carry different
            // messages and must each keep their own warning — the JSON
            // contract is one warning per `io_blocked[]` entry.
            let mut warnings = warnings;
            let mut blocked_sorted = io_blocked.clone();
            blocked_sorted.sort_by_key(|event| raw_path_sort_key(&event.path));
            blocked_sorted.dedup_by(|a, b| a.path == b.path);
            for event in &blocked_sorted {
                let (reason, code) = io_blocked_reason_and_code(event.reason);
                warnings.push(StatusWarning {
                    code,
                    message: format!(
                        "cannot inspect '{}': {reason}",
                        quote_pathname(&event.path, extras.quote_path)
                    ),
                    source: code.source(),
                });
            }
            let mut seen: HashSet<(StatusWarningCode, StatusWarningSource, String)> =
                HashSet::new();
            warnings.retain(|warning| {
                seen.insert((warning.code, warning.source, warning.message.clone()))
            });
            warnings
        },
        quote_path: extras.quote_path,
        io_blocked: {
            let mut events = io_blocked;
            events.sort_by_key(|event| raw_path_sort_key(&event.path));
            collapse_io_blocked_by_path(&mut events);
            events
        },
        base_scan_blocked,
        rename_scan_blocked,
    };
    filter_status_data_by_pathspec(&mut data, args)?;
    Ok(data)
}

/// Reattach a collapsed-directory trailing `/` marker that path projection
/// (`to_workdir_path`/`workdir_to_current`/`current_to_workdir`) normalizes
/// away — built from raw `OsString` bytes so a non-UTF-8 directory name
/// survives intact (never `display()`).
pub(super) fn with_dir_marker(path: &Path, projected: PathBuf) -> PathBuf {
    if path.as_os_str().as_encoded_bytes().ends_with(b"/")
        && !projected.as_os_str().as_encoded_bytes().ends_with(b"/")
    {
        let mut marker = projected.into_os_string();
        marker.push("/");
        PathBuf::from(marker)
    } else {
        projected
    }
}

impl StatusData {
    /// A copy whose change lists use repository-root-relative paths (the
    /// machine-format base, §B.6.4). Cheap enough for one render and
    /// keeps the human path base untouched.
    pub(super) fn to_repo_relative(&self) -> StatusData {
        /// Collapsed untracked/ignored directories carry a deliberate
        /// trailing `/` marker; `current_to_workdir` normalizes through
        /// path components and would eat it, making `?? dir/` render as
        /// `?? dir` — indistinguishable from an untracked FILE named `dir`.
        fn current_to_workdir_keeping_marker(path: &Path) -> PathBuf {
            with_dir_marker(path, current_to_workdir(path))
        }
        fn project(paths: &[PathBuf]) -> Vec<PathBuf> {
            paths
                .iter()
                .map(|path| current_to_workdir_keeping_marker(path))
                .collect()
        }
        fn project_changes(changes: &Changes) -> Changes {
            Changes {
                new: project(&changes.new),
                modified: project(&changes.modified),
                deleted: project(&changes.deleted),
                renamed: changes
                    .renamed
                    .iter()
                    .map(|(old, new)| {
                        (
                            current_to_workdir_keeping_marker(old),
                            current_to_workdir_keeping_marker(new),
                        )
                    })
                    .collect(),
            }
        }
        fn project_details(details: &RenameDetails) -> RenameDetails {
            details
                .iter()
                .map(|((old, new), value)| {
                    ((current_to_workdir(old), current_to_workdir(new)), *value)
                })
                .collect()
        }

        let mut projected = self.clone();
        projected.staged = project_changes(&self.staged);
        projected.unstaged = project_changes(&self.unstaged);
        projected.ignored_files = project(&self.ignored_files);
        projected.staged_rename_details = project_details(&self.staged_rename_details);
        projected.unstaged_rename_details = project_details(&self.unstaged_rename_details);
        for entry in &mut projected.unmerged {
            entry.path = current_to_workdir(&entry.path);
        }
        projected
    }
}

pub(super) fn filter_status_data_by_pathspec(
    data: &mut StatusData,
    args: &StatusArgs,
) -> CliResult<()> {
    if args.pathspec.is_empty() {
        return Ok(());
    }
    let pathspecs =
        PathspecSet::from_workdir(&args.pathspec, &util::cur_dir(), &util::working_dir())
            .map_err(pathspec_error_to_cli)?;

    filter_changes_by_pathspec(&mut data.staged, &pathspecs);
    filter_changes_by_pathspec(&mut data.unstaged, &pathspecs);
    // §B.3.2: a blocked path OUTSIDE the requested pathspec is not this
    // run's problem — it must neither fail a narrowed status closed nor
    // leak through `io_blocked[]`. The base-scan walk is pathspec-blind,
    // so the narrowing happens here, and the derived warnings follow.
    let blocked_before = data.io_blocked.len();
    // Keep an event when the path matches the spec OR could CONTAIN a match:
    // `:(glob)wanted/*.txt` never matches the directory `wanted`, yet a block
    // on that directory is exactly what hides the files the caller asked
    // for. The probe roots are already derived from the spec set, so a path
    // at, under, or above a root is in scope.
    let roots = crate::command::status_probe::pathspec_probe_roots(Some(&pathspecs));
    data.io_blocked.retain(|event| {
        if pathspecs.matches_path(&event.path) {
            return true;
        }
        let path = current_to_workdir(&event.path);
        roots.iter().any(|root| {
            root.as_os_str().is_empty() || path.starts_with(root) || root.starts_with(&path)
        })
    });
    if data.io_blocked.len() != blocked_before {
        // Only the warnings DERIVED from `io_blocked[]` are rebuilt. The
        // worktree family also carries aggregate rename-scoring warnings
        // (`worktree_read_failed` / `worktree_io_timeout` from
        // `warnings_from_rename_stats`) that name no path and are not tied
        // to any event — dropping those by source would hide a real
        // degradation, flip `rename_detection_complete` back to true, and
        // silently downgrade `--exit-code-on-warning` from 9.
        data.warnings
            .retain(|warning| !warning.message.starts_with("cannot inspect '"));
        let mut seen: HashSet<PathBuf> = HashSet::new();
        for event in &data.io_blocked {
            if !seen.insert(event.path.clone()) {
                continue;
            }
            let (reason, code) = io_blocked_reason_and_code(event.reason);
            data.warnings.push(StatusWarning {
                code,
                message: format!(
                    "cannot inspect '{}': {reason}",
                    quote_pathname(&event.path, data.quote_path)
                ),
                source: code.source(),
            });
        }
        if data.io_blocked.is_empty() {
            data.base_scan_blocked = false;
            // The rename-side flag survives unless BOTH sources of it are
            // gone: the blocked events (now empty) and the engine's own
            // aggregate warnings, which name no path and therefore are not
            // rebuilt above. Clearing it on event count alone would report
            // `rename_detection_complete = true` while an
            // "N candidate(s): worktree reads failed" warning is still in
            // the payload.
            data.rename_scan_blocked = data.warnings.iter().any(|warning| {
                matches!(
                    warning.source,
                    StatusWarningSource::Worktree | StatusWarningSource::Metadata
                ) && !warning.message.starts_with("cannot inspect '")
                    && warning.code != StatusWarningCode::UpstreamCountsUnavailable
            });
        }
    }
    data.unmerged
        .retain(|entry| current_relative_matches(&entry.path, &pathspecs));
    data.ignored_files
        .retain(|path| current_relative_matches(path, &pathspecs));
    if let Some(merge_state) = data.merge_state.as_mut() {
        merge_state
            .conflicted_paths
            .retain(|path| pathspecs.matches_path(Path::new(path)));
    }

    Ok(())
}

fn filter_changes_by_pathspec(changes: &mut Changes, pathspecs: &PathspecSet) {
    changes
        .new
        .retain(|path| current_relative_matches(path, pathspecs));
    changes
        .modified
        .retain(|path| current_relative_matches(path, pathspecs));
    changes
        .deleted
        .retain(|path| current_relative_matches(path, pathspecs));
    // Per-end pathspec semantics (§B.3): a rename pair survives only when
    // BOTH endpoints match. An old-only match demotes to a deletion and a
    // new-only match to an addition, so an out-of-scope endpoint can never
    // leak into the output through a rename record.
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (old, new) in changes.renamed.drain(..) {
        let old_in = current_relative_matches(&old, pathspecs);
        let new_in = current_relative_matches(&new, pathspecs);
        match (old_in, new_in) {
            (true, true) => kept.push((old, new)),
            (true, false) => changes.deleted.push(old),
            (false, true) => changes.new.push(new),
            (false, false) => {}
        }
    }
    changes.deleted.sort();
    changes.new.sort();
    changes.renamed = kept;
}

fn current_relative_matches(path: &Path, pathspecs: &PathspecSet) -> bool {
    pathspecs.matches_path(util::to_workdir_path(path))
}

fn pathspec_error_to_cli(error: PathspecError) -> CliError {
    match error {
        PathspecError::OutsideRepository { .. } => CliError::fatal(error.to_string())
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("all pathspecs must stay within the repository working tree"),
        PathspecError::UnsupportedMagic { .. } | PathspecError::InvalidPattern { .. } => {
            CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use supported magic: top, exclude, icase, literal, glob")
        }
    }
}

/// Where one side of a rename snapshot draws its blob identities from.
pub(super) enum RenameBlobSide<'a> {
    /// HEAD tree or index stage-0: repo-relative path → (oid, mode), a
    /// content-addressed fact (`KnownObjectId`, §B.4.1).
    Known(&'a HashMap<PathBuf, (ObjectHash, u32)>),
    /// The worktree: OID is streamed from the file during this call
    /// (`ComputedWorktreeThisCall`).
    Worktree,
}

/// Content provider for inexact scoring: HEAD/index blobs are read from the
/// object store by OID (de-duplicated, budgeted), worktree files are read
/// under the separate worktree budget (§B.7). The engine caches spanhashes
/// per path, so each path is requested at most once per side.
struct StatusContentSource {
    old_is_worktree: bool,
    new_is_worktree: bool,
    objects: rename_detect::ObjectReadBudget,
    worktree: rename_detect::WorktreeReadBudget,
}

impl StatusContentSource {
    fn read(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
        from_worktree: bool,
    ) -> rename_detect::ContentOutcome {
        use rename_detect::{BlobEvidence, ContentOutcome, SkipReason};
        match blob.evidence {
            BlobEvidence::KnownObjectId { oid } if !from_worktree => self.objects.read_blob(&oid),
            _ if from_worktree => {
                let abs = util::workdir_to_absolute(path);
                self.worktree.read_worktree_blob(&abs)
            }
            // A worktree-computed OID on the object side, or an Unknown blob:
            // no trustworthy object to read.
            _ => ContentOutcome::Skipped(SkipReason::ObjectUnavailable),
        }
    }
}

impl rename_detect::RenameContentSource for StatusContentSource {
    fn old_content(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        let from_worktree = self.old_is_worktree;
        self.read(path, blob, from_worktree)
    }

    fn new_content(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        let from_worktree = self.new_is_worktree;
        self.read(path, blob, from_worktree)
    }
}

/// Build one side of a [`rename_detect::RenameSnapshot`] from a change list.
///
/// `paths` are in the change list's own base (repo- or cwd-relative); each is
/// mapped to a repo-relative key via [`util::to_workdir_path`] so the HEAD/
/// index lookups and worktree reads are correct from any working directory
/// (fixing the historical subdirectory bug). The returned map is keyed by the
/// repo-relative path.
fn build_rename_side(
    paths: &[PathBuf],
    side: &RenameBlobSide<'_>,
    worktree_budget: &mut rename_detect::WorktreeReadBudget,
    snapshot_skips: &mut HashMap<rename_detect::SkipReason, u64>,
) -> HashMap<PathBuf, rename_detect::BlobRef> {
    use rename_detect::{BlobEvidence, BlobKind, BlobRef};
    // §B.4.1: the empty blob is recognizable by OID alone, so HEAD/index
    // sides can carry `size = Some(0)` (the engine's empty-file inexact
    // skip) without any object read; the constant follows the process hash
    // kind.
    let empty_blob_oid =
        git_internal::internal::object::blob::Blob::from_content_bytes(Vec::new()).id;
    let mut map = HashMap::new();
    for path in paths {
        let repo_key = util::to_workdir_path(path);
        let blob = match side {
            RenameBlobSide::Known(known) => {
                let Some((oid, mode)) = known.get(&repo_key).copied() else {
                    continue;
                };
                BlobRef {
                    kind: BlobKind::from_mode(mode),
                    mode,
                    size: (oid == empty_blob_oid).then_some(0),
                    evidence: BlobEvidence::KnownObjectId { oid },
                }
            }
            RenameBlobSide::Worktree => {
                let abs = util::workdir_to_absolute(&repo_key);
                // §B.3.3: this OPTIONAL stat runs under the deadline like
                // every other worktree read — a hung mount must cost the
                // rename candidate and a warning, not the whole command.
                // Debug-only seam for the scan→stat disappearance race: the
                // named path's stat is overridden with a genuine `NotFound`
                // error kind — the same branch an OS-level deletion drives,
                // without the hook mutating the worktree. Gated on
                // `LIBRA_TEST` like every seam in this family.
                #[cfg(debug_assertions)]
                let forced_vanish = std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
                    && std::env::var("LIBRA_TEST_VANISH_PATH")
                        .ok()
                        .filter(|target| !target.is_empty())
                        .is_some_and(|target| repo_key == std::path::Path::new(&target));
                #[cfg(not(debug_assertions))]
                let forced_vanish = false;
                let stat_target = abs.clone();
                // Bounded by the SHARED batch window, not the standalone
                // per-operation timeout: these preliminary stats are part of
                // the same §B.3.4 batch as the content reads, and giving
                // each its own 10s allowance would let snapshot construction
                // alone outlive the 5s deadline (2026-08-05 R0-1 review).
                let stat = crate::command::status_probe::with_io_deadline_bounded(
                    worktree_budget.read_window(),
                    move || stat_target.symlink_metadata(),
                )
                .unwrap_or_else(|()| Err(io::Error::new(io::ErrorKind::TimedOut, "reclaimed")));
                let stat = if forced_vanish {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "simulated vanish for the test seam",
                    ))
                } else {
                    stat
                };
                // Debug-only seam for the stat→hash race, which is far too
                // narrow to hit reliably from a test: the named path is
                // treated as having changed TYPE between the two reads.
                #[cfg(debug_assertions)]
                let forced_type_race = std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV)
                    .is_some()
                    && std::env::var("LIBRA_TEST_TYPE_RACE_PATH")
                        .ok()
                        .filter(|target| !target.is_empty())
                        .is_some_and(|target| repo_key == std::path::Path::new(&target));
                #[cfg(not(debug_assertions))]
                let forced_type_race = false;
                let stat_kind = stat.as_ref().err().map(|error| error.kind());
                let (kind, mode) = match stat {
                    Ok(meta) if meta.file_type().is_symlink() => (BlobKind::Symlink, 0o120000),
                    Ok(_) => (BlobKind::Regular, 0o100644),
                    Err(_) => {
                        // §B.3.4/§B.4.1: a candidate we cannot even stat is
                        // a DEGRADATION, not a silent non-candidate — the
                        // base status stays truthful, but the run must say
                        // rename detection was incomplete. That includes
                        // `NotFound`: the path DID exist when the scan
                        // enumerated it, so its disappearance is a race that
                        // cost a rename candidate, and reporting the run as
                        // complete would claim a pairing was ruled out when
                        // it was never attempted.
                        *snapshot_skips
                            .entry(if matches!(stat_kind, Some(io::ErrorKind::TimedOut)) {
                                rename_detect::SkipReason::IoTimeout
                            } else {
                                rename_detect::SkipReason::WorktreeIoFailed
                            })
                            .or_default() += 1;
                        continue;
                    }
                };
                // §B.3.4: worktree OID computation streams through the SAME
                // read budget that later feeds inexact content reads, so a
                // pathological candidate set cannot bypass the caps via the
                // exact stage (LFS paths hash the pointer blob, matching
                // what the index records).
                let (oid, size, observed_kind) =
                    match worktree_budget.worktree_blob_oid_and_size(&abs) {
                        Ok(triple) => triple,
                        Err(reason) => {
                            // Budget/size/I-O failures during the OPTIONAL
                            // worktree hash drop the candidate; record the
                            // reason so the same deduplicated warning family
                            // fires as for inexact content reads.
                            *snapshot_skips.entry(reason).or_default() += 1;
                            continue;
                        }
                    };
                // The kind was stat'ed above and the OID streamed just now.
                // If the path changed type in between, the OID describes
                // something the recorded kind does not — a symlink target
                // labelled `Regular` could then clear the exact gate against
                // a blob. Drop the candidate and report the race.
                if observed_kind != kind || forced_type_race {
                    *snapshot_skips
                        .entry(rename_detect::SkipReason::WorktreeIoFailed)
                        .or_default() += 1;
                    continue;
                }
                BlobRef {
                    kind,
                    mode,
                    size: Some(size),
                    evidence: BlobEvidence::ComputedWorktreeThisCall { oid },
                }
            }
        };
        map.insert(repo_key, blob);
    }
    map
}

/// The §B.7 comparison cap for `status`. Debug builds honor
/// `LIBRA_TEST_STATUS_COMPARISON_BUDGET` so a test can observe the shared
/// allowance without generating 500k real comparisons.
pub(super) fn status_comparison_budget() -> u64 {
    #[cfg(debug_assertions)]
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
        && let Ok(value) = std::env::var("LIBRA_TEST_STATUS_COMPARISON_BUDGET")
        && let Ok(parsed) = value.parse::<u64>()
        && parsed > 0
    {
        // Tighten-only, like the probe-limit seams: a test may shrink the
        // budget to force exhaustion, never raise it past production's cap.
        return parsed.min(rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS);
    }
    rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS
}

/// Call-level rename budget STATE, carried between detection sides.
///
/// §B.3.4/§B.7 specify ONE 500k comparison cap, ONE 64 MiB read cap and ONE
/// 5 s scoring deadline per `status` invocation. Rebuilding any of these per
/// side let a single call spend them twice while each side individually
/// looked compliant — so the remaining amounts, the ORIGINAL deadline, and
/// the OID de-duplication cache all travel between the passes.
pub(super) struct RenameBudgets {
    pub(super) objects_total: u64,
    pub(super) objects_slots: u32,
    pub(super) worktree_total: u64,
    pub(super) worktree_tasks: u32,
    /// Absolute batch deadline; a fresh one would hand the second side
    /// another full 5 s.
    deadline: std::time::Instant,
    /// Shared so an object both sides need is read once, not twice.
    pub(super) object_cache: Vec<(ObjectHash, Result<Vec<u8>, rename_detect::SkipReason>)>,
    /// Comparisons already spent; the next side's config is narrowed by it.
    pub(super) comparisons_spent: u64,
}

impl RenameBudgets {
    pub(super) fn new() -> Self {
        let objects = rename_detect::ObjectReadBudget::with_defaults();
        let worktree = rename_detect::WorktreeReadBudget::with_defaults();
        let (objects_total, objects_slots) = objects.remaining();
        let (worktree_total, worktree_tasks) = worktree.remaining();
        Self {
            objects_total,
            objects_slots,
            worktree_total,
            worktree_tasks,
            deadline: objects.deadline(),
            object_cache: Vec::new(),
            comparisons_spent: 0,
        }
    }

    fn take_objects(&mut self) -> rename_detect::ObjectReadBudget {
        rename_detect::ObjectReadBudget::resumed(
            self.objects_total,
            self.objects_slots,
            self.deadline,
            std::mem::take(&mut self.object_cache),
        )
    }

    fn take_worktree(&self) -> rename_detect::WorktreeReadBudget {
        rename_detect::WorktreeReadBudget::resumed(
            self.worktree_total,
            self.worktree_tasks,
            self.deadline,
        )
    }

    fn restore_objects(&mut self, budget: &mut rename_detect::ObjectReadBudget) {
        let (total, slots) = budget.remaining();
        self.objects_total = total;
        self.objects_slots = slots;
        self.object_cache = budget.take_cache();
    }

    fn restore_worktree(&mut self, budget: &rename_detect::WorktreeReadBudget) {
        let (total, tasks) = budget.remaining();
        self.worktree_total = total;
        self.worktree_tasks = tasks;
    }

    /// The config for the NEXT side, with its comparison allowance reduced
    /// by what earlier sides already spent.
    pub(super) fn narrowed(
        &self,
        config: &rename_detect::RenameDetectConfig,
    ) -> rename_detect::RenameDetectConfig {
        rename_detect::RenameDetectConfig {
            comparison_budget: config
                .comparison_budget
                .map(|budget| budget.saturating_sub(self.comparisons_spent)),
            ..config.clone()
        }
    }

    fn record_comparisons(&mut self, spent: u64) {
        self.comparisons_spent = self.comparisons_spent.saturating_add(spent);
    }
}

/// Per-pair rename detail: percentage score and exactness (§B.6.4/§B.6.5),
/// keyed by the display-base `(old, new)` pair recorded in `Changes.renamed`.
pub(super) type RenameDetails = HashMap<(PathBuf, PathBuf), (u32, bool)>;

/// Detect renames between the `deleted` (old) and `new` sides of `changes`
/// using the diffcore engine (exact by OID → unique basename → bounded
/// exhaustive inexact, §B.4.2). Matched pairs are recorded in
/// `changes.renamed` and pruned from `deleted`/`new`; each pair's score and
/// exactness are added to `details`. Paths keep the change list's original
/// base for display; detection runs on repo-relative keys.
/// Map rename-engine degradation stats onto structured warnings (§B.5).
/// Split out so the seam is unit-testable independent of read budgets.
/// Fold one detection side's stats into the run-wide accumulator.
fn merge_rename_stats(
    acc: &mut rename_detect::RenameDetectStats,
    side: &rename_detect::RenameDetectStats,
) {
    acc.comparisons += side.comparisons;
    acc.skipped_by_limit |= side.skipped_by_limit;
    acc.exhaustive_discarded |= side.exhaustive_discarded;
    acc.peak_edges = acc.peak_edges.max(side.peak_edges);
    for (reason, count) in &side.content_skips {
        *acc.content_skips.entry(*reason).or_default() += count;
    }
}

pub(super) fn warnings_from_rename_stats(
    stats: &rename_detect::RenameDetectStats,
    warnings: &mut Vec<StatusWarning>,
) {
    if stats.skipped_by_limit {
        warnings.push(StatusWarning {
            code: StatusWarningCode::RenameLimitProductSkipped,
            message: "rename detection skipped the exhaustive inexact pass: too many candidates on one side (renameLimit); exact and unique-basename matches were kept".to_string(),
            source: StatusWarningCode::RenameLimitProductSkipped.source(),
        });
    }
    if stats.exhaustive_discarded {
        warnings.push(StatusWarning {
            code: StatusWarningCode::SimilarityBudgetExceeded,
            message:
                "rename detection discarded the exhaustive inexact pass: similarity comparison budget exceeded; exact and already-scored unique-basename matches were kept"
                    .to_string(),
            source: StatusWarningCode::SimilarityBudgetExceeded.source(),
        });
    }
    // §B.3.4: content-read skips surface as deduplicated warnings — object
    // problems on the metadata side, worktree I/O on the worktree side,
    // budget/size caps as the budget family. Affected candidates were
    // dropped; the base status stays truthful.
    use rename_detect::SkipReason;
    let count = |reasons: &[SkipReason]| -> u64 {
        reasons
            .iter()
            .filter_map(|r| stats.content_skips.get(r))
            .sum()
    };
    // The reasons are side-qualified, so each family lands under the source
    // its published `source` claims: object-store problems under `metadata`,
    // working-tree problems under `worktree`. Folding them together would
    // report a worktree budget as a repository-object budget.
    let unavailable = count(&[
        SkipReason::ObjectMissing,
        SkipReason::ObjectCorrupt,
        SkipReason::ObjectUnavailable,
        SkipReason::ObjectIoFailed,
    ]);
    if unavailable > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::MetadataUnavailable,
            message: format!(
                "rename detection skipped {unavailable} candidate(s): repository objects missing, corrupt, or unreadable"
            ),
            source: StatusWarningCode::MetadataUnavailable.source(),
        });
    }
    let budget = count(&[SkipReason::ObjectTooLarge, SkipReason::ObjectBudgetExceeded]);
    if budget > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::MetadataBudgetExceeded,
            message: format!(
                "rename detection skipped {budget} candidate(s): object-read budget or per-object size cap reached"
            ),
            source: StatusWarningCode::MetadataBudgetExceeded.source(),
        });
    }
    let worktree_budget = count(&[
        SkipReason::WorktreeTooLarge,
        SkipReason::WorktreeBudgetExceeded,
    ]);
    if worktree_budget > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::WorktreeBudgetExceeded,
            message: format!(
                "rename detection skipped {worktree_budget} candidate(s): worktree-read budget or per-file size cap reached"
            ),
            source: StatusWarningCode::WorktreeBudgetExceeded.source(),
        });
    }
    let io_timeout = count(&[SkipReason::IoTimeout]);
    // NOTE: the worktree-family codes below are also emitted by the
    // io_blocked mapping for BASE-scan blocks, so `rename_detection_complete`
    // cannot key off the code alone — see `rename_scan_blocked`, which the
    // caller sets from these same counts.

    if io_timeout > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::WorktreeIoTimeout,
            message: format!(
                "rename detection reclaimed {io_timeout} candidate read(s) that exceeded the I/O deadline"
            ),
            source: StatusWarningCode::WorktreeIoTimeout.source(),
        });
    }
    let io_failed = count(&[SkipReason::WorktreeIoFailed]);
    if io_failed > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::WorktreeReadFailed,
            message: format!(
                "rename detection skipped {io_failed} candidate(s): worktree reads failed"
            ),
            source: StatusWarningCode::WorktreeReadFailed.source(),
        });
    }
}

/// R0-3: run unstaged rename detection against an EXPLICIT destination list
/// (the bounded probe's output, display base) instead of the untracked
/// display set. Matched pairs land in `changes.renamed`; the returned set
/// holds the consumed destinations for §B.3.5 marker collapse.
pub(super) fn detect_renames_with_destinations(
    changes: &mut Changes,
    config: &rename_detect::RenameDetectConfig,
    old_side: RenameBlobSide<'_>,
    destinations_display: &[PathBuf],
    details: &mut RenameDetails,
    stats_acc: &mut rename_detect::RenameDetectStats,
    budgets: &mut RenameBudgets,
) -> HashSet<PathBuf> {
    let mut consumed_new: HashSet<PathBuf> = HashSet::new();
    if changes.deleted.is_empty() || destinations_display.is_empty() {
        return consumed_new;
    }
    let mut worktree_budget = budgets.take_worktree();
    let mut snapshot_skips: HashMap<rename_detect::SkipReason, u64> = HashMap::new();
    let snapshot = rename_detect::RenameSnapshot {
        old_map: build_rename_side(
            &changes.deleted,
            &old_side,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
        new_map: build_rename_side(
            destinations_display,
            &RenameBlobSide::Worktree,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
    };
    let mut source = StatusContentSource {
        old_is_worktree: matches!(old_side, RenameBlobSide::Worktree),
        new_is_worktree: true,
        objects: budgets.take_objects(),
        worktree: worktree_budget,
    };
    let narrowed = budgets.narrowed(config);
    let mut outcome = rename_detect::match_pairs(&snapshot, &narrowed, &mut source);
    // Hand the drawn-down budgets back, exactly like the sibling detector:
    // the run-level caps are call-level (§B.3.4), so this pass must neither
    // keep the remainder nor spend comparisons off the books — a pass added
    // after this one would otherwise restart with fresh budgets.
    budgets.restore_objects(&mut source.objects);
    budgets.restore_worktree(&source.worktree);
    budgets.record_comparisons(outcome.stats.comparisons);
    // Snapshot-construction skips (optional worktree hash/stat failures)
    // join the engine's own content skips so ONE warning family covers
    // every candidate the run had to drop.
    for (reason, count) in snapshot_skips {
        *outcome.stats.content_skips.entry(reason).or_default() += count;
    }
    // The stats are ACCUMULATED, not turned into warnings here: staged and
    // unstaged detection run separately, and emitting per side would produce
    // two warnings with the same {code, source} and different counts —
    // §B.5 requires exactly one per code/source for the whole run.
    merge_rename_stats(stats_acc, &outcome.stats);
    if outcome.matches.is_empty() {
        return consumed_new;
    }

    let mut consumed_old: HashSet<PathBuf> = HashSet::new();
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    for m in &outcome.matches {
        let old_display = util::workdir_to_current(&m.old);
        let new_display = util::workdir_to_current(&m.new);
        consumed_old.insert(old_display.clone());
        consumed_new.insert(new_display.clone());
        details.insert(
            (old_display.clone(), new_display.clone()),
            (m.score_percent(), m.exact),
        );
        renamed.push((old_display, new_display));
    }
    changes.deleted.retain(|p| !consumed_old.contains(p));
    changes.new.retain(|p| !consumed_new.contains(p));
    changes.deleted.sort();
    changes.new.sort();
    renamed.sort_by(|a, b| a.1.cmp(&b.1));
    changes.renamed.extend(renamed);
    consumed_new
}

fn detect_renames_in_changes(
    changes: &mut Changes,
    config: &rename_detect::RenameDetectConfig,
    old_side: RenameBlobSide<'_>,
    new_side: RenameBlobSide<'_>,
    details: &mut RenameDetails,
    stats_acc: &mut rename_detect::RenameDetectStats,
    budgets: &mut RenameBudgets,
) {
    if changes.deleted.is_empty() || changes.new.is_empty() {
        return;
    }
    // Budgets are CALL-level, not per-side: the staged and unstaged passes
    // draw from the same caps. Constructing them here let one `status` spend
    // 2 × 500k comparisons and 2 × 64 MiB of reads while both sides
    // individually looked compliant.
    let mut worktree_budget = budgets.take_worktree();
    let mut snapshot_skips: HashMap<rename_detect::SkipReason, u64> = HashMap::new();
    let snapshot = rename_detect::RenameSnapshot {
        old_map: build_rename_side(
            &changes.deleted,
            &old_side,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
        new_map: build_rename_side(
            &changes.new,
            &new_side,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
    };
    let mut source = StatusContentSource {
        old_is_worktree: matches!(old_side, RenameBlobSide::Worktree),
        new_is_worktree: matches!(new_side, RenameBlobSide::Worktree),
        objects: budgets.take_objects(),
        worktree: worktree_budget,
    };
    let narrowed = budgets.narrowed(config);
    let mut outcome = rename_detect::match_pairs(&snapshot, &narrowed, &mut source);
    // Hand the drawn-down budgets back so the OTHER side continues from
    // here rather than starting fresh.
    budgets.restore_objects(&mut source.objects);
    budgets.restore_worktree(&source.worktree);
    budgets.record_comparisons(outcome.stats.comparisons);
    for (reason, count) in snapshot_skips {
        *outcome.stats.content_skips.entry(reason).or_default() += count;
    }
    // The stats are ACCUMULATED, not turned into warnings here: staged and
    // unstaged detection run separately, and emitting per side would produce
    // two warnings with the same {code, source} and different counts —
    // §B.5 requires exactly one per code/source for the whole run.
    merge_rename_stats(stats_acc, &outcome.stats);
    if outcome.matches.is_empty() {
        return;
    }

    // Map repo-relative matches back to the change list's display base and
    // prune consumed endpoints.
    let mut consumed_old: HashSet<PathBuf> = HashSet::new();
    let mut consumed_new: HashSet<PathBuf> = HashSet::new();
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    for m in &outcome.matches {
        let old_display = util::workdir_to_current(&m.old);
        let new_display = util::workdir_to_current(&m.new);
        consumed_old.insert(old_display.clone());
        consumed_new.insert(new_display.clone());
        details.insert(
            (old_display.clone(), new_display.clone()),
            (m.score_percent(), m.exact),
        );
        renamed.push((old_display, new_display));
    }
    changes.deleted.retain(|p| !consumed_old.contains(p));
    changes.new.retain(|p| !consumed_new.contains(p));
    changes.deleted.sort();
    changes.new.sort();
    renamed.sort_by(|a, b| a.1.cmp(&b.1));
    changes.renamed.extend(renamed);
}

/// Numeric Unix mode for a `TreeItemMode` (`100644`/`100755`/`120000`/
/// `160000`), matching Git's stored blob modes.
fn tree_item_mode_to_unix(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Commit => 0o160000,
        TreeItemMode::Tree => 0o040000,
    }
}

/// The error for a HEAD object that passes ref validation but is not in the
/// object store. `Commit::load` / `Tree::load` PANIC in that case, so every
/// status path that expands HEAD goes through the `try_load` pair below: a
/// pruned or corrupt object is a repository problem the user can act on, not
/// a reason to take the process down.
pub(crate) fn head_object_unreadable(what: &str, oid: &ObjectHash) -> CliError {
    CliError::fatal(format!(
        "cannot read the HEAD {what} '{oid}': the object is missing or corrupt"
    ))
    .with_stable_code(StableErrorCode::RepoStateInvalid)
    .with_hint("run 'libra fsck' or restore the object, then retry")
}

/// Load the HEAD commit and its tree, failing closed when either is absent.
pub(crate) fn load_head_commit_tree(head_oid: &ObjectHash) -> CliResult<(Commit, Tree)> {
    let commit =
        Commit::try_load(head_oid).ok_or_else(|| head_object_unreadable("commit", head_oid))?;
    let tree = Tree::try_load(&commit.tree_id)
        .ok_or_else(|| head_object_unreadable("tree", &commit.tree_id))?;
    Ok((commit, tree))
}

/// HEAD tree blobs keyed by repo-relative path → (oid, mode) (§B.4.1 old side
/// of the staged snapshot).
fn load_head_tree_blobs(head_oid: &ObjectHash) -> CliResult<HashMap<PathBuf, (ObjectHash, u32)>> {
    let (_, tree) = load_head_commit_tree(head_oid)?;
    Ok(tree
        .get_plain_items_with_mode()
        .into_iter()
        .map(|(path, hash, mode)| (path, (hash, tree_item_mode_to_unix(mode))))
        .collect())
}

/// Index stage-0 blobs keyed by repo-relative path → (oid, mode) (index side
/// of both snapshots).
fn load_index_stage0_blobs(index: &Index) -> HashMap<PathBuf, (ObjectHash, u32)> {
    index
        .tracked_entries(0)
        .into_iter()
        .map(|entry| (PathBuf::from(&entry.name), (entry.hash, entry.mode)))
        .collect()
}

pub(crate) fn load_status_index() -> CliResult<Index> {
    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    Index::load(&index_path).map_err(|source| {
        CliError::from(StatusError::IndexLoad {
            path: index_path,
            source,
        })
    })
}
