//! Diff comparison/selection: revision/scan/algorithm resolution, side
//! hydration, pathspec filtering and rename detection.
#![allow(unused_imports)]
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write as _,
    io::{self, IsTerminal},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use git_internal::{
    Diff,
    hash::{HashKind, ObjectHash, set_hash_kind},
    internal::{
        index::{Index, IndexEntry, Time},
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
            types::ObjectType,
        },
        pack::utils::calculate_object_hash,
    },
};
use serde::Serialize;
use similar::{Algorithm, ChangeTag, TextDiff};
use tempfile::NamedTempFile;

#[cfg(test)]
use self::options::parse_rename_score;
use self::options::{DiffPrefixes, ResolvedDiffConfig, resolve_diff_config};
use super::*;
use crate::{
    command::{
        load_object, read_worktree_blob_bytes,
        unmerged::{self, UnmergedEntry},
    },
    internal::{config::ConfigKv, head::Head},
    utils::{
        attributes,
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        ignore::{self, IgnorePolicy},
        output::{ColorChoice, OutputConfig, ProgressMode},
        path,
        pathspec::{PathspecError, PathspecSet},
        preview_object, util,
    },
};

pub(crate) async fn run_diff(
    args: &DiffArgs,
    output: &OutputConfig,
    config: &ResolvedDiffConfig,
    pickaxe: Option<&DiffPickaxe>,
    diff_algorithm: &DiffAlgorithm,
) -> Result<DiffOutput, DiffError> {
    util::require_repo().map_err(|_| DiffError::NotInRepo)?;
    tracing::debug!("diff args: {:?}", args);
    let index = Index::load(path::index()).map_err(|e| DiffError::IndexLoad(e.to_string()))?;
    // ADR-FM-05: whether working-tree mode differences count is controlled by
    // core.fileMode (invalid values fail diff closed before any output).
    let file_mode = crate::internal::config::core_file_mode()
        .await
        .map_err(|error| DiffError::InvalidConfig(error.to_string()))?;

    // `--progress=json` keeps immediate NDJSON scan events for machine
    // consumers. Gate matches the old startup print: --json output, --quiet,
    // --staged, and rev-vs-rev comparisons never emit scan progress (#466).
    let scan_progress_eligible =
        !output.is_json() && !output.quiet && !args.staged && args.new.is_none();
    if matches!(output.progress, ProgressMode::Json) && scan_progress_eligible {
        let event = serde_json::json!({
            "event": "diff_scan.start",
            "task": "Scanning working tree",
        });
        eprintln!("{event}");
    }
    // The deferred text hint only guards the real worktree scan (the unstaged
    // new side): a HEAD/index/rev load is not what the line describes.
    let mut scan_hint = if scan_progress_eligible {
        WorktreeScanHint::start(output)
    } else {
        WorktreeScanHint {
            enabled: false,
            done: true,
            shown: false,
            #[cfg(test)]
            sink_override: None,
            #[cfg(test)]
            test_tty: None,
        }
    };
    // The unstaged worktree scan is pure synchronous index/worktree I/O with
    // no await point, so a plain `tokio::time::timeout` around an inline
    // future could never fire mid-scan (the future completes in one poll).
    // Run it on the blocking pool and race its handle against the quiet
    // period: a fast scan wins silently; a slow one lets the timer reveal
    // the hint mid-scan, and the SAME handle is awaited afterwards — the
    // scan runs exactly once and a huge tree is never truncated (#466,
    // cf. #372). `finish()` erases the hint when the scan completes.
    let old_side =
        resolve_diff_side(&args.old, args.staged, false, &index, file_mode, false).await?;
    let (mut new_side, index) = if scan_hint.enabled {
        // `Index` is not `Clone`; move it into the blocking task through an
        // `Arc` and take it back out afterwards.
        // The hash kind lives in a git-internal THREAD-LOCAL seeded by the
        // `core.objectformat` preflight on the main thread; blocking-pool
        // threads start at the default (SHA-1). Capture the kind here and
        // re-seed it inside the task, or SHA-256 repositories would have
        // every stat-miss file re-hashed with the wrong algorithm.
        let hash_kind = git_internal::hash::get_hash_kind();
        let plumbing_stat_only = args.plumbing_stat_only;
        let shared_index = Arc::new(index);
        let mut scan_handle = {
            let index = Arc::clone(&shared_index);
            tokio::task::spawn_blocking(move || {
                // Re-seed this pool thread's thread-local (defaults to SHA-1).
                // Restored via `Drop` so a panicking scan cannot leave the
                // wrong kind behind (mirrors `HashKindGuard`, which is
                // test-only in git-internal).
                let _kind_guard = SetHashKindGuard {
                    previous: git_internal::hash::get_hash_kind(),
                };
                set_hash_kind(hash_kind);
                resolve_worktree_side(&index, file_mode, plumbing_stat_only)
            })
        };
        let side =
            race_scan_with_hint(&mut scan_handle, WORKTREE_SCAN_QUIET_PERIOD, &mut scan_hint)
                .await
                .map_err(|error| DiffError::FileRead {
                    path: "working tree scan".to_string(),
                    detail: format!("worktree scan task panicked or was cancelled: {error}"),
                })??;
        scan_hint.finish();
        // The handle has completed, so this task owns the last reference;
        // recover the index for the later unmerged-paths pass. (If the
        // runtime somehow kept a task-side clone alive, fall back to a fresh
        // empty index — apply_unmerged_worktree_diff then no-ops on an empty
        // stage-0 set rather than misreporting.)
        let index = match Arc::try_unwrap(shared_index) {
            Ok(index) => index,
            // Unreachable: the completed scan was the only other reference.
            Err(_) => Index::new(),
        };
        (side, index)
    } else {
        let side = resolve_diff_side(
            &args.new,
            args.staged,
            true,
            &index,
            file_mode,
            args.plumbing_stat_only,
        )
        .await?;
        (side, index)
    };

    // A plumbing patch renders clean content but retains its stat candidates
    // for raw/name output and exit status, including an empty patch with exit 1.
    new_side.worktree_entries.clean |= args.plumbing_stat_only && patch_body_is_shown(args);

    let pathspecs =
        PathspecSet::from_workdir(&args.pathspec, &util::cur_dir(), &util::working_dir())
            .map_err(pathspec_error_to_diff)?;
    let paths: Vec<PathBuf> = pathspecs.plain_positive_prefixes().unwrap_or_default();
    let diff_pathspecs = paths.clone();
    let worktree_entries = new_side.worktree_entries.clone();
    // Separate copy for content post-passes (the one above is moved into the diff
    // closure below). Directional worktree identity for raw/external metadata is
    // carried explicitly below so same-content mode changes cannot zero both sides.
    let ext_worktree_entries = new_side.worktree_entries.clone();
    let old_modes = old_side.modes;
    let new_modes = new_side.modes;
    let old_is_worktree = old_side.is_worktree;
    let new_is_worktree = new_side.is_worktree;
    // `Rc` so the `-U<n>` post-pass can read the blob content the diff closure
    // cached (keyed by path and hash) without re-loading it from the object store/disk.
    let worktree_cache: WorktreeContentCache = Rc::new(RefCell::new(HashMap::new()));
    let repo_cache: Rc<RefCell<HashMap<ObjectHash, Vec<u8>>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let worktree_cache_in = Rc::clone(&worktree_cache);
    let repo_cache_in = Rc::clone(&repo_cache);
    let load_error = Rc::new(RefCell::new(None::<DiffError>));
    let load_error_for_read = Rc::clone(&load_error);
    // `-R`/`--reverse`: swap the two sides so the diff is computed new->old. The
    // loader resolves blobs by hash (content-addressed) and the worktree check
    // above stays correct regardless of which side a blob lands on.
    let (
        first_blobs,
        second_blobs,
        first_modes,
        second_modes,
        first_is_worktree,
        second_is_worktree,
        old_label,
        new_label,
    ) = if args.reverse {
        (
            new_side.blobs,
            old_side.blobs,
            new_modes,
            old_modes,
            new_is_worktree,
            old_is_worktree,
            new_side.label,
            old_side.label,
        )
    } else {
        (
            old_side.blobs,
            new_side.blobs,
            old_modes,
            new_modes,
            old_is_worktree,
            new_is_worktree,
            old_side.label,
            new_side.label,
        )
    };
    // Path → blob-hash for each side (in the diff direction git_internal uses),
    // captured before the blobs are moved into `Diff::diff`, so the `-U<n>`
    // post-pass can look up each file's old/new content from the caches.
    let first_map: HashMap<PathBuf, ObjectHash> = first_blobs.iter().cloned().collect();
    let second_map: HashMap<PathBuf, ObjectHash> = second_blobs.iter().cloned().collect();
    if preview_object::is_active() {
        let storage = ClientStorage::try_init(path::objects())?;
        let mut hashes = HashSet::new();
        for (path, hash) in &first_map {
            if second_map.get(path) != Some(hash) {
                hashes.insert(*hash);
            }
        }
        for (path, hash) in &second_map {
            if first_map.get(path) != Some(hash) {
                hashes.insert(*hash);
            }
        }
        let hashes: Vec<ObjectHash> = hashes
            .into_iter()
            .filter(|hash| !preview_object::contains(hash))
            .collect();
        let remaining =
            preview_object::remaining_cache_bytes().map_err(|error| DiffError::FileRead {
                path: "commit preview object batch".to_string(),
                detail: error.to_string(),
            })?;
        let sized = preflight_preview_object_sizes(hashes, |hashes| {
            storage.object_sizes_with_total_limit(hashes, remaining)
        })?;
        for (hash, size) in sized {
            preview_object::reserve(hash, size).map_err(|error| DiffError::FileRead {
                path: format!("commit preview object {hash}"),
                detail: error.to_string(),
            })?;
        }
    }
    let diff_output = Diff::diff(first_blobs, second_blobs, paths, move |path, hash| {
        if worktree_entries.get(path) == Some(hash) {
            if let Some(data) = worktree_cache_in
                .borrow()
                .get(&(path.clone(), *hash))
                .cloned()
            {
                return data;
            }

            match worktree_entries.read(path, hash) {
                Ok(data) => {
                    worktree_cache_in
                        .borrow_mut()
                        .insert((path.clone(), *hash), data.clone());
                    data
                }
                Err(err) => {
                    record_diff_content_error(&load_error_for_read, err);
                    Vec::new()
                }
            }
        } else {
            if let Some(data) = repo_cache_in.borrow().get(hash).cloned() {
                return data;
            }

            match load_repo_blob_content(hash) {
                Ok(data) => {
                    repo_cache_in.borrow_mut().insert(*hash, data.clone());
                    data
                }
                Err(err) => {
                    record_diff_content_error(&load_error_for_read, err);
                    Vec::new()
                }
            }
        }
    });
    if let Some(err) = load_error.borrow_mut().take() {
        return Err(err);
    }

    let mut files: Vec<DiffFileStat> = diff_output.iter().map(parse_diff_item).collect();
    if args.plumbing_stat_only && patch_body_is_shown(args) {
        for file in &mut files {
            let path = PathBuf::from(&file.path);
            let old_hash = first_map.get(&path);
            let new_hash = second_map.get(&path);
            let cache = worktree_cache.borrow();
            let objects = repo_cache.borrow();
            let content = |hash: Option<&ObjectHash>, worktree: bool| {
                hash.and_then(|hash| {
                    if worktree {
                        cache.get(&(path.clone(), *hash))
                    } else {
                        objects.get(hash)
                    }
                })
            };
            let old = content(old_hash, first_is_worktree);
            let new = content(new_hash, second_is_worktree);
            if old.is_some() && old == new && first_modes.get(&path) == second_modes.get(&path) {
                // Preserve the entry for stat-based raw output and exit status.
                file.raw_diff.clear();
                file.hunks.clear();
                file.insertions = 0;
                file.deletions = 0;
                continue;
            }
            let render_hash =
                |hash: Option<&ObjectHash>, bytes: Option<&Vec<u8>>, worktree: bool| {
                    let hash = if worktree {
                        bytes.map(|bytes| calculate_object_hash(ObjectType::Blob, bytes))
                    } else {
                        hash.copied()
                    };
                    hash.map(|hash| hash.to_string()[..7].to_string())
                        .unwrap_or_else(|| "0000000".to_string())
                };
            let old_hex = render_hash(old_hash, old, first_is_worktree);
            let new_hex = render_hash(new_hash, new, second_is_worktree);
            file.raw_diff = file
                .raw_diff
                .lines()
                .map(|line| {
                    if let Some(rest) = line.strip_prefix("index ") {
                        let suffix = rest.find(' ').map(|pos| &rest[pos..]).unwrap_or("");
                        format!("index {old_hex}..{new_hex}{suffix}\n")
                    } else {
                        format!("{line}\n")
                    }
                })
                .collect();
        }
    }
    append_mode_only_changes(
        &mut files,
        &first_map,
        &second_map,
        &first_modes,
        &second_modes,
    );
    if args.old.is_none() && args.new.is_none() && !args.staged {
        apply_unmerged_worktree_diff(&mut files, &index, &diff_pathspecs)?;
    }
    filter_diff_files_by_pathspec(&mut files, &pathspecs);

    // Resolve the external diff driver (`diff.external`) when it should drive this
    // run: a patch-body output mode (not `--stat`/name/numstat/summary/`-s`/
    // `--check`), human/file output (not `--json`/`--quiet`), and not disabled by
    // `--no-ext-diff`. When active it REPLACES the patch entirely (applied after
    // the internal post-passes below, which are then skipped), matching Git.
    let external_command: Option<String> =
        if !args.no_ext_diff && !output.is_json() && !output.quiet && patch_body_is_shown(args) {
            ConfigKv::get("diff.external")
                .await
                .ok()
                .flatten()
                .map(|entry| entry.value)
                .filter(|cmd| !cmd.trim().is_empty())
        } else {
            None
        };

    // Post-pass regeneration (both reuse the blob text the diff closure cached —
    // keyed by path/hash for worktree content — with no re-load; the default path leaves git_internal's
    // output untouched):
    //   * A whitespace-ignoring flag (`-w`/`-b`/`--ignore-space-at-eol`) re-diffs
    //     each text file through the matching line normalizer, DROPS files whose
    //     only change is whitespace under that rule, and recomputes that file's
    //     +/- counts (so stat/name/numstat/JSON all reflect the result).
    //   * `--ignore-blank-lines` re-diffs ignoring blank-only changes (drops files
    //     whose only change is blank lines, recomputes counts).
    //   * Patience/Histogram replaces git_internal's initial Myers body and
    //     recomputes +/- counts; the same backend flows through the two filtered
    //     paths above, rename/textconv bodies, and forced-text rendering.
    //   * `-U<n>` (when `n != 3`, git_internal's hard-coded default) regenerates
    //     hunk bodies at `n` context lines; +/- lines are unchanged so counts are
    //     untouched — only the surrounding context (and re-parsed `hunks`) change.
    // The re-diff flags honor `-U<n>` for context width; `-w` > `-b` >
    // `--ignore-space-at-eol` if more than one is given (matching Git).
    // `--ignore-blank-lines` COMPOSES with a whitespace flag: the diff and the
    // blank classification both run through the normalizer (matching Git).
    let regen_context = config.context;
    let requested_ws_normalize: Option<fn(&str) -> String> = if args.ignore_all_space {
        Some(normalize_ignore_all_space)
    } else if args.ignore_space_change {
        Some(normalize_ignore_space_change)
    } else if args.ignore_space_at_eol {
        Some(normalize_ignore_space_at_eol)
    } else if args.ignore_cr_at_eol {
        Some(normalize_ignore_cr_at_eol)
    } else {
        None
    };
    // `--check` ignores comparison filters but still honors an explicitly
    // selected diff backend because ambiguous repeated lines can change which
    // physical lines are classified as additions.
    let ws_normalize = if args.check {
        None
    } else {
        requested_ws_normalize
    };
    let ignore_blank = !args.check && args.ignore_blank_lines;
    let rediffs = ws_normalize.is_some() || ignore_blank || diff_algorithm.needs_backend_rediff();

    // `--relative` restricts WHICH files are diffed; apply that restriction now —
    // before rename detection — so a rename pair is only formed when BOTH sides
    // lie inside the prefix, matching Git (which filters before diffcore-rename).
    // A pair straddling the boundary therefore stays an add or a delete. The
    // path-rewriting half runs later (`apply_relative_filter`, or skipped for
    // verbatim external output).
    if let Some(strip) = relative_prefix(args) {
        files.retain(|file| file.path.starts_with(&strip));
    }

    // `-M`/`--find-renames`: fold matched delete+add pairs into single rename
    // entries. Done here (after the whitespace/context selection, before the
    // post-passes) so the rename's own content diff honors `-U<n>`/`-w`/blank
    // rules and the post-passes then leave rename entries alone.
    if let Some(threshold) = config.rename_threshold {
        // `--check` scans added lines for whitespace errors and ignores the
        // whitespace-ignore flags, so the rename body must stay unfiltered.
        let rename_skips = apply_rename_detection(
            &mut files,
            &first_map,
            &second_map,
            &first_modes,
            &second_modes,
            &ext_worktree_entries,
            threshold,
            config.rename_limit,
            config.rename_comparison_budget,
            regen_context,
            ws_normalize,
            ignore_blank,
            diff_algorithm,
        )?;
        if rename_skips.inexact_skipped_by_limit {
            crate::utils::error::emit_legacy_stderr(format!(
                "warning: skipped inexact rename detection because more than {} sources or destinations changed (diff.renameLimit); exact and unique-basename renames were still detected",
                config.rename_limit,
            ));
        }
        if rename_skips.inexact_discarded_by_budget {
            crate::utils::error::emit_legacy_stderr(
                "warning: inexact rename detection exceeded diff.renameComparisonBudget; the exhaustive pass was discarded and only exact and unique-basename renames were detected",
            );
        }
    }

    populate_diff_metadata(
        &mut files,
        &first_map,
        &second_map,
        &first_modes,
        &second_modes,
        first_is_worktree,
        second_is_worktree,
    );
    for file in &mut files {
        apply_mode_metadata_to_patch(file);
        fix_empty_side_labels(file);
    }
    // issues/479 DF-01 / TC-0044: the status letter must follow ENTRY PRESENCE,
    // not the rendered patch text. `parse_diff_item` classifies from the
    // header, where a side whose content is empty is rendered as `/dev/null`;
    // an empty blob is still an existing entry, so a stat-invalid worktree
    // entry over an empty index blob used to be reported as `A` (and a
    // worktree copy truncated to zero bytes as `D`). Git reports every
    // stat-invalid entry as `M`, so the authoritative object metadata wins
    // whenever both sides' presence is known.
    refresh_status_from_modes(&mut files);

    // Textconv (`--textconv`, on by default unless `--no-textconv`): re-diff the
    // output of each file's `diff.<driver>.textconv` command instead of the raw
    // bytes. Skipped under `--check` (it scans raw added lines) and when an
    // external driver is active (that takes precedence). The post-pass below then
    // leaves textconv'd files alone.
    let textconv_outcome = if !args.no_textconv && !args.check && external_command.is_none() {
        let mut command_cache: HashMap<String, Option<String>> = HashMap::new();
        // Per file: the (old-side, new-side) textconv command. A rename's
        // old side is at `rename_from` and may resolve a different driver
        // than the new side (Git resolves textconv per blob/path), so each
        // side is looked up independently.
        let mut path_commands: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for file in &files {
            let new_path = PathBuf::from(&file.path);
            let old_path = file
                .rename_from
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| new_path.clone());
            let new_driver = attributes::diff_driver_for_path(&new_path);
            let new_command =
                resolve_textconv_command(new_driver.as_deref(), &mut command_cache).await;
            let old_command = if old_path == new_path {
                new_command.clone()
            } else {
                let old_driver = attributes::diff_driver_for_path(&old_path);
                resolve_textconv_command(old_driver.as_deref(), &mut command_cache).await
            };
            if old_command.is_some() || new_command.is_some() {
                path_commands.insert(file.path.clone(), (old_command, new_command));
            }
        }
        if path_commands.is_empty() {
            TextconvOutcome::default()
        } else {
            apply_textconv(
                &mut files,
                &path_commands,
                &first_map,
                &second_map,
                &ext_worktree_entries,
                regen_context,
                ws_normalize,
                ignore_blank,
                diff_algorithm,
                match pickaxe {
                    Some(DiffPickaxe::StringCount(needle)) => Some(needle.as_slice()),
                    _ => None,
                },
            )?
        }
    } else {
        TextconvOutcome::default()
    };
    let textconv_paths = &textconv_outcome.paths;

    // Binary detection: a file whose content carries a NUL byte is shown as
    // `Binary files … differ` (or, with `--binary`, a `GIT binary patch`) instead
    // of a content diff. `--text` forces the content diff; `--check` and an active
    // external driver take over the body, and textconv'd files are already text.
    // The context/whitespace post-pass below then skips binary files.
    let mut binary_patch = false;
    if !args.text && !args.check && external_command.is_none() {
        binary_patch = apply_binary_detection(
            &mut files,
            &first_map,
            &second_map,
            &ext_worktree_entries,
            textconv_paths,
            args.binary,
        )?;
    } else if args.text && !args.check && external_command.is_none() {
        // `--text` forces content even for non-UTF-8 files git_internal already
        // collapsed to a bare `Binary files differ`.
        force_text_for_bare_binary(
            &mut files,
            &first_map,
            &second_map,
            &ext_worktree_entries,
            regen_context,
            diff_algorithm,
        )?;
    }

    // `--binary` implies `--full-index`: rewrite every applicable `index` line
    // to full object ids. Binary-patch entries already carry full ids; ordinary
    // binary markers still need rewriting when `--full-index` is explicit.
    if (args.binary || args.full_index) && external_command.is_none() {
        for file in files.iter_mut() {
            // Binary files were already given full ids (with the correct
            // blank-line terminator) in `apply_binary_detection`; don't re-process.
            if args.binary && file.binary.is_some() {
                continue;
            }
            let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
            let old_id = first_map
                .get(&PathBuf::from(old_path))
                .map(|h| h.to_string());
            let new_id = second_map
                .get(&PathBuf::from(&file.path))
                .map(|h| h.to_string());
            let width = old_id
                .as_ref()
                .or(new_id.as_ref())
                .map(String::len)
                .unwrap_or(40);
            let zeros = "0".repeat(width);
            file.raw_diff = binary_index_full(
                &file.raw_diff,
                &old_id.unwrap_or_else(|| zeros.clone()),
                &new_id.unwrap_or(zeros),
            );
        }
    }

    // `--check` ignores whitespace/blank-line filters, matching Git, but an
    // explicitly selected Patience/Histogram backend still regenerates the
    // body before the added-line scan.
    if external_command.is_none() && (rediffs || (!args.check && regen_context != 3)) {
        let blob_text = |map: &HashMap<PathBuf, ObjectHash>, path: &Path| -> String {
            let Some(hash) = map.get(path) else {
                return String::new();
            };
            // Clone out of each borrow so no reference escapes the temporary `Ref`.
            let bytes = worktree_cache
                .borrow()
                .get(&(path.to_path_buf(), *hash))
                .cloned()
                .or_else(|| repo_cache.borrow().get(hash).cloned());
            bytes
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        if rediffs {
            files.retain_mut(|file| {
                // Rename and textconv entries already carry their final rendered
                // body (textconv re-diffs the converted content at this context),
                // so leave them untouched by the whitespace/context re-diff.
                if file.status == "renamed" || textconv_paths.contains(&file.path) {
                    return true;
                }
                // Binary / no-hunk diffs have no body to re-diff: keep as-is.
                if !file.raw_diff.contains("\n@@ ") {
                    return true;
                }
                let path = PathBuf::from(&file.path);
                let old_text = blob_text(&first_map, &path);
                let new_text = blob_text(&second_map, &path);
                // `--ignore-blank-lines` composes with a whitespace normalizer when
                // both are given (matching `git diff -w --ignore-blank-lines`).
                let body = if ignore_blank {
                    match ws_normalize {
                        Some(normalize) => compute_unified_hunks_ignore_blank_normalized(
                            &old_text,
                            &new_text,
                            regen_context,
                            diff_algorithm,
                            normalize,
                        ),
                        None => compute_unified_hunks_ignore_blank(
                            &old_text,
                            &new_text,
                            regen_context,
                            diff_algorithm,
                        ),
                    }
                } else if let Some(normalize) = ws_normalize {
                    compute_unified_hunks_normalized(
                        &old_text,
                        &new_text,
                        regen_context,
                        diff_algorithm,
                        normalize,
                    )
                } else {
                    compute_unified_hunks(&old_text, &new_text, regen_context, diff_algorithm)
                };
                // No change survives the rule. Git still reports an added/deleted
                // filepair or mode change (header, zero counts, no hunk) even when
                // its only content is blank lines — only a content-only
                // modification disappears entirely.
                if body.trim().is_empty() {
                    // `file.status` is parsed only from the pre-hunk header lines
                    // (`parse_diff_status` stops at the first `@@`), so a body line
                    // that merely contains "new file mode" cannot misclassify a
                    // modification as an add/delete.
                    let keep_header = file.status == "added"
                        || file.status == "deleted"
                        || matches!(
                            (file.old_mode, file.new_mode),
                            (Some(old), Some(new)) if old != new
                        );
                    if !keep_header {
                        return false;
                    }
                    file.insertions = 0;
                    file.deletions = 0;
                    file.hunks = Vec::new();
                    file.raw_diff = strip_unified_diff_body(&file.raw_diff);
                    return true;
                }
                let (insertions, deletions) = count_body_changes(&body);
                file.insertions = insertions;
                file.deletions = deletions;
                file.raw_diff = splice_unified_body(&file.raw_diff, &body);
                file.hunks = parse_diff_hunks(&file.raw_diff);
                true
            });
        } else {
            for file in files.iter_mut() {
                // Rename entries already rendered their content diff at the
                // requested context in `build_rename_entry`; do not re-diff them
                // (their old side is at `rename_from`, not `file.path`). Textconv'd
                // files were likewise already re-diffed at this context, and binary
                // files have no text body.
                if file.status == "renamed"
                    || textconv_paths.contains(&file.path)
                    || file.binary.is_some()
                {
                    continue;
                }
                let path = PathBuf::from(&file.path);
                let old_text = blob_text(&first_map, &path);
                let new_text = blob_text(&second_map, &path);
                file.raw_diff = rewrite_unified_diff_context(
                    &file.raw_diff,
                    &old_text,
                    &new_text,
                    regen_context,
                    diff_algorithm,
                );
                file.hunks = parse_diff_hunks(&file.raw_diff);
            }
        }
    }

    apply_pickaxe(
        &mut files,
        pickaxe,
        &first_map,
        &second_map,
        &ext_worktree_entries,
        &textconv_outcome.pickaxe_counts,
    )?;

    if let Some(filter) = parse_diff_filter(args.diff_filter.as_deref())? {
        apply_diff_filter(&mut files, &filter);
    }

    // Apply the external diff driver LAST so its verbatim output is never touched
    // by the internal post-passes (skipped above) or the later word-diff pass
    // (skipped in `execute_safe` via `external_diff_applied`).
    let external_diff_applied = if let Some(command) = &external_command {
        // The `--relative` file-set restriction was already applied above (before
        // rename detection); the path-rewriting half stays skipped for verbatim
        // driver output, so the driver only sees files inside the prefix.
        apply_external_diff(
            &mut files,
            command,
            &first_map,
            &second_map,
            first_is_worktree,
            second_is_worktree,
        )?;
        true
    } else {
        false
    };

    if args.check {
        annotate_diff_check_trailing_blanks(
            &mut files,
            &second_map,
            &ext_worktree_entries,
            &worktree_cache,
            &repo_cache,
        )?;
    }

    let total_insertions = files.iter().map(|file| file.insertions).sum();
    let total_deletions = files.iter().map(|file| file.deletions).sum();
    let files_changed = files.len();

    Ok(DiffOutput {
        old_ref: old_label,
        new_ref: new_label,
        files,
        total_insertions,
        total_deletions,
        files_changed,
        external_diff_applied,
        binary_patch,
    })
}

pub(crate) fn filter_diff_files_by_pathspec(
    files: &mut Vec<DiffFileStat>,
    pathspecs: &PathspecSet,
) {
    if pathspecs.is_empty() {
        return;
    }
    files.retain(|file| {
        pathspecs.matches_path(&file.path)
            || file
                .rename_from
                .as_ref()
                .is_some_and(|old_path| pathspecs.matches_path(old_path))
    });
}

pub(crate) fn populate_diff_metadata(
    files: &mut [DiffFileStat],
    old_blobs: &HashMap<PathBuf, ObjectHash>,
    new_blobs: &HashMap<PathBuf, ObjectHash>,
    old_modes: &HashMap<PathBuf, u32>,
    new_modes: &HashMap<PathBuf, u32>,
    old_is_worktree: bool,
    new_is_worktree: bool,
) {
    for file in files {
        if file.raw_diff.starts_with("diff --cc ") {
            continue;
        }
        let old_path = PathBuf::from(file.rename_from.as_deref().unwrap_or(&file.path));
        let new_path = PathBuf::from(&file.path);
        file.old_id = (!old_is_worktree)
            .then(|| old_blobs.get(&old_path).copied())
            .flatten();
        file.new_id = (!new_is_worktree)
            .then(|| new_blobs.get(&new_path).copied())
            .flatten();
        file.old_mode = old_modes.get(&old_path).copied();
        file.new_mode = new_modes.get(&new_path).copied();
    }
}

/// Derive each entry's status letter from the object metadata instead of the
/// rendered patch text (issues/479 DF-01 / TC-0044).
///
/// `parse_diff_item` classifies from the patch header, where `git_internal`
/// renders a side whose *content* is empty as `/dev/null`. An empty blob is
/// still an existing entry, so an empty tracked file whose stat information
/// was invalidated (`read-tree`, `reset --mixed`, checkout, ...) was reported
/// as `A`, and a worktree copy truncated to zero bytes as `D`. Git classifies
/// by entry presence: both sides present is `M`, including the "stat-invalid"
/// case where the worktree object id is reported as the all-zero hash without
/// reading (hashing) its content.
///
/// Only entries whose presence metadata is authoritative are rewritten.
/// Unmerged (`diff --cc`), detected renames, and entries without any mode
/// metadata keep the text-derived status.
pub(crate) fn refresh_status_from_modes(files: &mut [DiffFileStat]) {
    for file in files.iter_mut() {
        if file.status == "renamed" || file.raw_diff.starts_with("diff --cc ") {
            continue;
        }
        let derived = match (file.old_mode, file.new_mode) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            // Both sides exist: modified (a type change is still `M` here;
            // `diff_status_code` upgrades the letter to `T`).
            (Some(_), Some(_)) => "modified",
            (None, None) => continue,
        };
        file.status = derived.to_string();
    }
}

pub(crate) fn apply_mode_metadata_to_patch(file: &mut DiffFileStat) {
    if file.raw_diff.starts_with("diff --cc ") {
        return;
    }
    let trailing_newline = file.raw_diff.ends_with('\n');
    let mode_change = match (file.old_mode, file.new_mode) {
        (Some(old), Some(new)) if old != new => Some((old, new)),
        _ => None,
    };
    let mut output = Vec::new();
    for (index, line) in file.raw_diff.lines().enumerate() {
        if index == 1
            && let Some((old, new)) = mode_change
        {
            output.push(format!("old mode {old:06o}"));
            output.push(format!("new mode {new:06o}"));
        }
        if line.starts_with("old mode ") || line.starts_with("new mode ") {
            continue;
        }
        if line.starts_with("new file mode ")
            && let Some(mode) = file.new_mode
        {
            output.push(format!("new file mode {mode:06o}"));
            continue;
        }
        if line.starts_with("deleted file mode ")
            && let Some(mode) = file.old_mode
        {
            output.push(format!("deleted file mode {mode:06o}"));
            continue;
        }
        if let Some(ids) = line
            .strip_prefix("index ")
            .and_then(|rest| rest.split_whitespace().next())
        {
            let rewritten = match (file.old_mode, file.new_mode) {
                (Some(old), Some(new)) if old == new => format!("index {ids} {new:06o}"),
                _ => format!("index {ids}"),
            };
            output.push(rewritten);
            continue;
        }
        output.push(line.to_string());
    }
    file.raw_diff = output.join("\n");
    if trailing_newline {
        file.raw_diff.push('\n');
    }
}

/// ADR-LG-04 item 2: only a truly deleted file renders `+++ /dev/null`.
/// `git_internal` renders a side whose *content* is empty as `/dev/null`, so an
/// emptied-but-present file would otherwise look deleted. Presence metadata
/// (`old_mode`/`new_mode`) is authoritative here: rewrite the labels back to
/// `a/<path>` / `b/<path>` when both sides exist.
pub(crate) fn fix_empty_side_labels(file: &mut DiffFileStat) {
    if file.raw_diff.starts_with("diff --cc ") {
        return;
    }
    if !matches!((file.old_mode, file.new_mode), (Some(_), Some(_))) {
        return;
    }
    let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
    let mut changed = false;
    let repaired: Vec<String> = file
        .raw_diff
        .lines()
        .map(|line| {
            if line == "--- /dev/null" {
                changed = true;
                return format!("--- a/{old_path}");
            }
            if line == "+++ /dev/null" {
                changed = true;
                return format!("+++ b/{}", file.path);
            }
            line.to_string()
        })
        .collect();
    if changed {
        let mut raw = repaired.join("\n");
        if file.raw_diff.ends_with('\n') {
            raw.push('\n');
        }
        file.raw_diff = raw;
        // The header changed, so the parsed hunk boundaries must follow.
        file.hunks = parse_diff_hunks(&file.raw_diff);
    }
}

pub(crate) fn get_worktree_diff_files(index: &Index) -> Result<Vec<PathBuf>, DiffError> {
    let mut files = Vec::new();

    for file in index.tracked_files() {
        // ADR-SW-04 item 1: skip-worktree paths are sparse-checkout entries;
        // their (absent or stale) worktree copy is not a diff.
        if file
            .to_str()
            .and_then(|name| index.get(name, 0))
            .is_some_and(|entry| entry.flags.skip_worktree)
        {
            continue;
        }
        let absolute = util::workdir_to_absolute(&file);
        if std::fs::symlink_metadata(&absolute).is_ok() {
            files.push(file);
        }
    }

    Ok(files)
}

pub(crate) fn get_index_side(
    index: &Index,
    policy: IgnorePolicy,
    exclude_skip_worktree: bool,
) -> (Vec<(PathBuf, ObjectHash)>, HashMap<PathBuf, u32>) {
    let entries = index
        .tracked_entries(0)
        .into_iter()
        // WT-06 (ADR-WT-05): an intent-to-add entry is an empty-blob
        // PLACEHOLDER for a new file, not index content. Git never shows it
        // on the index side: `diff`/`diff-files`/`diff HEAD` render it as a
        // new file, and `diff --cached` omits it entirely.
        .filter(|entry| !entry.flags.intent_to_add)
        // ADR-SW-04 item 1: a skip-worktree path is intentionally absent or
        // stale in the worktree and must not appear on the index side of a
        // WORKING-DIRECTORY diff. A staged diff (`--cached`) still shows the
        // index content, so the caller opts in explicitly.
        .filter(|entry| !exclude_skip_worktree || !entry.flags.skip_worktree)
        .filter(|entry| !ignore::should_ignore(&PathBuf::from(&entry.name), policy, index));
    let mut blobs = Vec::new();
    let mut modes = HashMap::new();
    for entry in entries {
        let path = PathBuf::from(&entry.name);
        blobs.push((path.clone(), entry.hash));
        modes.insert(path, entry.mode);
    }
    (blobs, modes)
}

pub(crate) fn get_worktree_modes(files: &[PathBuf]) -> Result<HashMap<PathBuf, u32>, DiffError> {
    files
        .iter()
        .map(|path| {
            let absolute = util::workdir_to_absolute(path);
            let metadata =
                std::fs::symlink_metadata(&absolute).map_err(|error| DiffError::FileRead {
                    path: absolute.display().to_string(),
                    detail: error.to_string(),
                })?;
            Ok((path.clone(), index_mode_from_metadata(&metadata)))
        })
        .collect()
}

pub(crate) fn resolve_worktree_side(
    index: &Index,
    file_mode: bool,
    stat_only: bool,
) -> Result<DiffSide, DiffError> {
    let files = get_worktree_diff_files(index)?;
    let blobs = if stat_only {
        get_files_blobs_stat_only(&files, index, IgnorePolicy::Respect)?
    } else {
        get_files_blobs(&files, index, IgnorePolicy::Respect)?
    };
    // ADR-FM-05: with core.fileMode=false the worktree mode comparison is
    // disabled; the index's recorded mode is used so only content (and entry
    // type) changes surface.
    let modes = if file_mode {
        get_worktree_modes(&files)?
    } else {
        files
            .iter()
            .map(|path| {
                let mode = path
                    .to_str()
                    .and_then(|name| index.get(name, 0))
                    .map(|entry| entry.mode)
                    .unwrap_or(0o100644);
                (path.clone(), mode)
            })
            .collect()
    };
    Ok(DiffSide {
        label: "working tree".to_string(),
        worktree_entries: WorktreeContents {
            entries: blobs.iter().cloned().collect(),
            selected_index: crate::command::status_io_worker::selected_index_oids(index),
            clean: !stat_only,
        },
        blobs,
        modes,
        is_worktree: true,
    })
}

pub(crate) async fn resolve_diff_side(
    source: &Option<String>,
    staged: bool,
    is_new: bool,
    index: &Index,
    file_mode: bool,
    stat_only: bool,
) -> Result<DiffSide, DiffError> {
    if let Some(source) = source {
        let (blobs, modes) = get_treeish_entries(source).await?;
        return Ok(DiffSide {
            label: source.clone(),
            blobs,
            modes,
            worktree_entries: WorktreeContents::default(),
            is_worktree: false,
        });
    }

    if is_new {
        if staged {
            let (blobs, modes) = get_index_side(index, IgnorePolicy::Respect, false);
            Ok(DiffSide {
                label: "index".to_string(),
                blobs,
                modes,
                worktree_entries: WorktreeContents::default(),
                is_worktree: false,
            })
        } else {
            resolve_worktree_side(index, file_mode, stat_only)
        }
    } else if staged {
        match Head::current_commit().await {
            Some(commit_hash) => {
                let (blobs, modes) = get_commit_entries(&commit_hash).await?;
                Ok(DiffSide {
                    label: "HEAD".to_string(),
                    blobs,
                    modes,
                    worktree_entries: WorktreeContents::default(),
                    is_worktree: false,
                })
            }
            None => Ok(DiffSide {
                label: "HEAD".to_string(),
                blobs: Vec::new(),
                modes: HashMap::new(),
                worktree_entries: WorktreeContents::default(),
                is_worktree: false,
            }),
        }
    } else {
        let (blobs, modes) = get_index_side(index, IgnorePolicy::Respect, true);
        Ok(DiffSide {
            label: "index".to_string(),
            blobs,
            modes,
            worktree_entries: WorktreeContents::default(),
            is_worktree: false,
        })
    }
}

pub(crate) async fn get_commit_blobs(
    commit_hash: &ObjectHash,
) -> Result<Vec<(PathBuf, ObjectHash)>, DiffError> {
    get_commit_entries(commit_hash)
        .await
        .map(|(blobs, _)| blobs)
}

pub(crate) async fn get_commit_entries(
    commit_hash: &ObjectHash,
) -> Result<DiffTreeEntries, DiffError> {
    let commit = load_object::<Commit>(commit_hash).map_err(|e| DiffError::ObjectLoad {
        kind: "commit",
        object_id: commit_hash.to_string(),
        detail: e.to_string(),
    })?;
    get_tree_entries(&commit.tree_id)
}

pub(crate) async fn get_treeish_entries(source: &str) -> Result<DiffTreeEntries, DiffError> {
    let tree_id = util::resolve_tree_ish_with_auto_merge_typed(source)
        .await
        .map_err(|_| DiffError::InvalidRevision(source.to_string()))?;
    get_tree_entries(&tree_id)
}

pub(crate) fn get_tree_entries(tree_id: &ObjectHash) -> Result<DiffTreeEntries, DiffError> {
    let tree = load_object::<Tree>(tree_id).map_err(|e| DiffError::ObjectLoad {
        kind: "tree",
        object_id: tree_id.to_string(),
        detail: e.to_string(),
    })?;
    let mut blobs = Vec::new();
    let mut modes = HashMap::new();
    collect_tree_entries(&tree, Path::new(""), &mut blobs, &mut modes)?;
    Ok((blobs, modes))
}

pub(crate) fn collect_tree_entries(
    tree: &Tree,
    prefix: &Path,
    blobs: &mut Vec<(PathBuf, ObjectHash)>,
    modes: &mut HashMap<PathBuf, u32>,
) -> Result<(), DiffError> {
    for item in &tree.tree_items {
        let item_path = prefix.join(&item.name);
        match item.mode {
            TreeItemMode::Tree => {
                let subtree =
                    load_object::<Tree>(&item.id).map_err(|error| DiffError::ObjectLoad {
                        kind: "tree",
                        object_id: item.id.to_string(),
                        detail: error.to_string(),
                    })?;
                collect_tree_entries(&subtree, &item_path, blobs, modes)?;
            }
            TreeItemMode::Commit => {
                crate::utils::error::emit_legacy_stderr(format!(
                    "Warning: Submodule '{}' is not supported yet; skipping checkout entry",
                    item_path.display()
                ));
            }
            mode => {
                blobs.push((item_path.clone(), item.id));
                modes.insert(item_path, tree_mode_to_index_mode(mode));
            }
        }
    }
    Ok(())
}

pub(crate) async fn diff_stat_between_commits(
    old_commit: &ObjectHash,
    new_commit: &ObjectHash,
) -> Result<String, DiffError> {
    let old_blobs = get_commit_blobs(old_commit).await?;
    let new_blobs = get_commit_blobs(new_commit).await?;

    // Capture the first blob-read failure from the (infallible-signature) diff
    // closure and surface it after, mirroring `run_diff`.
    let load_error: RefCell<Option<DiffError>> = RefCell::new(None);
    let diff_output =
        Diff::diff(
            old_blobs,
            new_blobs,
            Vec::new(),
            |_path, hash| match load_repo_blob_content(hash) {
                Ok(data) => data,
                Err(err) => {
                    if load_error.borrow().is_none() {
                        *load_error.borrow_mut() = Some(err);
                    }
                    Vec::new()
                }
            },
        );
    if let Some(err) = load_error.borrow_mut().take() {
        return Err(err);
    }

    let files: Vec<DiffFileStat> = diff_output.iter().map(parse_diff_item).collect();
    let total_insertions = files.iter().map(|file| file.insertions).sum();
    let total_deletions = files.iter().map(|file| file.deletions).sum();
    let files_changed = files.len();
    let output = DiffOutput {
        old_ref: old_commit.to_string(),
        new_ref: new_commit.to_string(),
        files,
        total_insertions,
        total_deletions,
        files_changed,
        external_diff_applied: false,
        binary_patch: false,
    };
    Ok(format_diff_stat_output(&output))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_rename_detection(
    files: &mut Vec<DiffFileStat>,
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    first_modes: &HashMap<PathBuf, u32>,
    second_modes: &HashMap<PathBuf, u32>,
    worktree_entries: &WorktreeContents,
    threshold: u32,
    rename_limit: usize,
    comparison_budget: Option<u64>,
    context: usize,
    ws_normalize: Option<fn(&str) -> String>,
    ignore_blank: bool,
    diff_algorithm: &DiffAlgorithm,
) -> Result<RenameDetectionSkips, DiffError> {
    // NOTE: file-type eligibility (same kind for exact, non-empty regular
    // files for inexact) now lives in the shared engine, which is the whole
    // point of routing through it — these rules used to be duplicated here
    // and drifted from `status`.

    // Indices of the deleted (old-only) and added (new-only) entries.
    let deleted: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].status == "deleted")
        .collect();
    let added: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].status == "added")
        .collect();
    if deleted.is_empty() || added.is_empty() {
        return Ok(RenameDetectionSkips::default());
    }

    // ── Pairing is delegated to the SHARED engine ────────────────────────
    //
    // `diff` used to carry its own exact/basename/limit/top-K/greedy
    // implementation. Two matchers meant two sets of tie-breaks, two budget
    // accountings and two eligibility rules, and they drifted: the same
    // repository could report different renames depending on which command
    // you asked. Everything below is translation — build the snapshot, run
    // `match_pairs`, map the result back to `files` indices.
    let path_index: HashMap<PathBuf, usize> = deleted
        .iter()
        .chain(added.iter())
        .map(|&i| (PathBuf::from(&files[i].path), i))
        .collect();

    // The engine's inexact stage is restricted to NON-EMPTY regular files
    // (`size != Some(0)`). Leaving every size `None` would let empty blobs
    // reach content scoring here but not in `status`, so a small
    // `diff.renameComparisonBudget` could be spent on them and a real
    // rename discarded — the same input producing two different answers.
    // Recorded ids are enough to recognise emptiness without a read.
    let empty_oid = git_internal::internal::object::blob::Blob::from_content_bytes(Vec::new()).id;
    let blob_for = |path: &str,
                    map: &HashMap<PathBuf, ObjectHash>,
                    modes: &HashMap<PathBuf, u32>|
     -> Option<rename_detect::BlobRef> {
        let pb = PathBuf::from(path);
        let oid = *map.get(&pb)?;
        let mode = modes.get(&pb).copied().unwrap_or(0o100644);
        Some(rename_detect::BlobRef {
            kind: rename_detect::BlobKind::from_mode(mode),
            mode,
            size: (oid == empty_oid).then_some(0),
            // Both sides are recorded tree/index/worktree ids, so exact
            // pairing is permitted (§B.4.1 allow-list).
            evidence: rename_detect::BlobEvidence::KnownObjectId { oid },
        })
    };

    let mut snapshot = rename_detect::RenameSnapshot::default();
    for &di in &deleted {
        if let Some(blob) = blob_for(&files[di].path, first_map, first_modes) {
            snapshot
                .old_map
                .insert(PathBuf::from(&files[di].path), blob);
        }
    }
    for &ai in &added {
        if let Some(blob) = blob_for(&files[ai].path, second_map, second_modes) {
            snapshot
                .new_map
                .insert(PathBuf::from(&files[ai].path), blob);
        }
    }

    /// Content provider over diff's loaders. Object (non-worktree) reads go
    /// through [`ObjectReadBudget`] so WIO-03's cancellable 5s deadline
    /// applies (a hung store cannot stall diff), but WITHOUT status's
    /// object-count/byte caps: diff has no comparison budget by contract
    /// (plan-20260714 §B.7, pinned by `diff_no_comparison_budget_regression`),
    /// and the 64-object status cap silently unpaired every rename past the
    /// first 64 blobs (pre-WIO diff read blobs uncapped).
    struct DiffContentSource<'a> {
        first_map: &'a HashMap<PathBuf, ObjectHash>,
        second_map: &'a HashMap<PathBuf, ObjectHash>,
        worktree_entries: &'a WorktreeContents,
        objects: rename_detect::ObjectReadBudget,
        worktree_error: Option<DiffError>,
    }

    impl DiffContentSource<'_> {
        fn read(
            &mut self,
            path: &Path,
            map: &HashMap<PathBuf, ObjectHash>,
        ) -> rename_detect::ContentOutcome {
            let Some(hash) = map.get(path) else {
                return rename_detect::ContentOutcome::Skipped(
                    rename_detect::SkipReason::ObjectMissing,
                );
            };
            if self.worktree_entries.get(path) == Some(hash) {
                return match self.worktree_entries.read(path, hash) {
                    Ok(bytes) => rename_detect::ContentOutcome::Content(std::rc::Rc::new(bytes)),
                    Err(error) => {
                        self.worktree_error = Some(error);
                        rename_detect::ContentOutcome::Skipped(
                            rename_detect::SkipReason::ObjectUnavailable,
                        )
                    }
                };
            }
            // Commit dry-run preview blobs live only in scratch storage —
            // consult that before the worker, which sees `.libra/objects` only.
            // Repository fallbacks still go through ObjectReadBudget so a
            // hung store cannot stall `commit --dry-run --verbose` (WIO-03).
            if let Ok(Some(content)) = preview_object::read(hash) {
                return rename_detect::ContentOutcome::Content(std::rc::Rc::new(content));
            }
            match self.objects.read_blob_tracked(hash) {
                (rename_detect::ContentOutcome::Content(bytes), _) => {
                    rename_detect::ContentOutcome::Content(bytes)
                }
                // A worker-COMPLETED skip means the store outcome is known
                // and the object is one the worker structurally cannot
                // serve — it is local-only and frame-capped (8 MiB), so
                // remote-only objects under tiered storage and blobs past
                // the frame cap silently unpaired renames pre-WIO diff
                // handled (§B.7). Those fall back to the legacy in-process
                // tiered read (pre-WIO parity, no deadline for this
                // residual class). A TRANSPORT skip (worker killed at the
                // deadline, spawn/dispatch/protocol failure) carries no
                // store outcome — the killable bound stays final there
                // (WIO-03, Codex r9): retrying it in-process would reopen
                // exactly the unbounded stall the worker exists to bound.
                (
                    rename_detect::ContentOutcome::Skipped(reason),
                    rename_detect::ObjectReadProvenance::Completed,
                ) => match load_repo_blob_content(hash) {
                    Ok(bytes) => rename_detect::ContentOutcome::Content(std::rc::Rc::new(bytes)),
                    Err(_) => rename_detect::ContentOutcome::Skipped(reason),
                },
                (
                    skip @ rename_detect::ContentOutcome::Skipped(_),
                    rename_detect::ObjectReadProvenance::Transport,
                ) => skip,
            }
        }
    }

    impl rename_detect::RenameContentSource for DiffContentSource<'_> {
        fn old_content(
            &mut self,
            path: &Path,
            _blob: &rename_detect::BlobRef,
        ) -> rename_detect::ContentOutcome {
            self.read(path, self.first_map)
        }

        fn new_content(
            &mut self,
            path: &Path,
            _blob: &rename_detect::BlobRef,
        ) -> rename_detect::ContentOutcome {
            self.read(path, self.second_map)
        }
    }

    let mut source = DiffContentSource {
        first_map,
        second_map,
        worktree_entries,
        worktree_error: None,
        // Deadline-only budget: keep the killable worker read (WIO-03) but
        // no per-object/total/object-count caps — see DiffContentSource docs.
        objects: rename_detect::ObjectReadBudget::new(
            u64::MAX,
            u64::MAX,
            u32::MAX,
            rename_detect::OBJECT_READ_DEADLINE,
        ),
    };
    let config = rename_detect::RenameDetectConfig {
        threshold,
        rename_limit,
        comparison_budget,
    };
    let outcome = rename_detect::match_pairs(&snapshot, &config, &mut source);
    if let Some(error) = source.worktree_error {
        return Err(error);
    }

    // Rendering must reuse the same killable ObjectReadBudget (WIO-03): a
    // second legacy `load_repo_blob_content` pass would stall on a hung store
    // after detection already succeeded.
    let mut objects = source.objects;
    let mut load =
        |path: &str, map: &HashMap<PathBuf, ObjectHash>| -> Result<Option<Vec<u8>>, DiffError> {
            let pb = PathBuf::from(path);
            let Some(hash) = map.get(&pb) else {
                return Ok(None);
            };
            if worktree_entries.get(&pb) == Some(hash) {
                return worktree_entries.read(&pb, hash).map(Some);
            }
            if let Ok(Some(content)) = preview_object::read(hash) {
                return Ok(Some(content));
            }
            Ok(match objects.read_blob_tracked(hash) {
                (rename_detect::ContentOutcome::Content(bytes), _) => Some((*bytes).clone()),
                // Same fallback rule as detection (see DiffContentSource): only
                // a worker-COMPLETED skip may recover through the legacy tiered
                // read; a transport skip stays final (WIO-03, Codex r9).
                (
                    rename_detect::ContentOutcome::Skipped(_),
                    rename_detect::ObjectReadProvenance::Completed,
                ) => load_repo_blob_content(hash).ok(),
                (
                    rename_detect::ContentOutcome::Skipped(_),
                    rename_detect::ObjectReadProvenance::Transport,
                ) => None,
            })
        };

    let mut pairs: Vec<(usize, usize, u32)> = Vec::new();
    for matched in &outcome.matches {
        let (Some(&di), Some(&ai)) = (path_index.get(&matched.old), path_index.get(&matched.new))
        else {
            continue;
        };
        pairs.push((di, ai, matched.internal_score));
    }
    let inexact_skipped = outcome.stats.skipped_by_limit;
    let budget_discarded = outcome.stats.exhaustive_discarded;

    let skips = RenameDetectionSkips {
        inexact_skipped_by_limit: inexact_skipped,
        inexact_discarded_by_budget: budget_discarded,
    };
    if pairs.is_empty() {
        return Ok(skips);
    }

    // Build the rename entries, then drop the consumed del/add entries.
    let mut renames: HashMap<usize, DiffFileStat> = HashMap::with_capacity(pairs.len());
    for (di, ai, score) in &pairs {
        let old_path = files[*di].path.clone();
        let new_path = files[*ai].path.clone();
        let percent = score / 600;
        let old_content = load(&old_path, first_map)?.unwrap_or_default();
        let new_content = load(&new_path, second_map)?.unwrap_or_default();
        let entry = build_rename_entry(
            &old_path,
            &new_path,
            percent,
            first_map.get(&PathBuf::from(&old_path)),
            second_map.get(&PathBuf::from(&new_path)),
            &old_content,
            &new_content,
            context,
            ws_normalize,
            ignore_blank,
            diff_algorithm,
        );
        // Insert at the added entry's position so output order stays stable.
        renames.insert(*ai, entry);
    }
    let drop: std::collections::HashSet<usize> =
        pairs.iter().flat_map(|(d, a, _)| [*d, *a]).collect();
    let mut rebuilt: Vec<DiffFileStat> = Vec::with_capacity(files.len());
    for (idx, file) in files.drain(..).enumerate() {
        if let Some(rename) = renames.remove(&idx) {
            rebuilt.push(rename);
        } else if !drop.contains(&idx) {
            rebuilt.push(file);
        }
    }
    *files = rebuilt;
    Ok(skips)
}
