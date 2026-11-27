//! Merge facade/presentation: CLI output rendering, error-to-CliError mapping,
//! merge message/signature helpers.
#![allow(unused_imports)]
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

// Preserve the existing command::merge type path for downstream callers.
#[allow(unused_imports)]
pub(crate) use autostash::StoppedMerge;
pub(crate) use autostash::{
    MergeAutostash, conclude_merge_after_commit, conclude_stopped_merge, snapshot_stopped_merge,
};
use autostash::{
    preflight_held_autostash, prepare_merge_autostash, resolve_pending_autostash,
    resolve_pending_autostash_with, store_pending_autostash, verify_autostash_ownership,
};
use clap::{Parser, ValueEnum};
pub(crate) use conflict::*;
pub(crate) use content::*;
use git_internal::{
    hash::ObjectHash,
    internal::{
        index::Index,
        object::{
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
};
pub(crate) use rename_merge::*;
use serde::{Deserialize, Serialize};
pub(crate) use state::{
    MergeState, merge_in_progress, merge_state_for_pseudo_refs, merge_state_gc_oids,
};
pub(crate) use tree_merge::*;
pub(crate) use virtual_base::*;
pub(crate) use workdir::*;

use super::{
    get_target_commit, load_object, load_object_raw, rename_detect, reset,
    restore::{self, RestoreArgs},
    save_object, status, switch, *,
};
use crate::{
    command::{
        commit::{CleanupMode, cleanup_commit_message, parse_cleanup_mode},
        editor,
    },
    common_utils::{format_commit_msg, parse_commit_msg},
    info_println,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        merge_base,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        repo_hooks::{
            RepoHook, replay_repo_hook_output, run_advisory_repo_hook, run_repo_hook_with_io,
        },
        tree_plumbing,
    },
    utils::{
        attributes::{self, AttributeState},
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        path, util, worktree,
    },
};

pub(crate) fn signing_policy_from_options(
    options: &PullMergeOptions,
) -> Result<crate::command::history_config::CommitSigningPolicy, PullMergeError> {
    options.signing_policy.ok_or_else(|| {
        PullMergeError::History(
            "merge signing policy was not resolved before commit construction".to_string(),
        )
    })
}

pub(crate) fn merge_message_path() -> Result<PathBuf, PullMergeError> {
    util::try_get_worktree_gitdir(None)
        .map(|gitdir| gitdir.join("COMMIT_EDITMSG"))
        .map_err(|error| PullMergeError::MessageFileWrite {
            path: ".libra/COMMIT_EDITMSG".to_string(),
            detail: format!("failed to locate the current worktree metadata directory: {error}"),
        })
}

pub(crate) fn write_merge_message(path: &Path, message: &str) -> Result<(), PullMergeError> {
    crate::utils::atomic_write::write_atomic(path, message.as_bytes(), false).map_err(|error| {
        PullMergeError::MessageFileWrite {
            path: path.display().to_string(),
            detail: error.to_string(),
        }
    })
}

pub(crate) fn read_merge_message(path: &Path) -> Result<String, PullMergeError> {
    fs::read_to_string(path).map_err(|error| PullMergeError::MessageFileRead {
        path: path.display().to_string(),
        detail: error.to_string(),
    })
}

pub(crate) fn render_merge_output(result: &MergeOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("merge", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    let selected_strategy = result
        .selected_strategy
        .as_deref()
        .unwrap_or(result.strategy.as_str());

    if result.dry_run {
        // `--dry-run`: preview phrasing — nothing was written, so the normal
        // messages ("Fast-forward", "fix conflicts and then commit") would be
        // misleading or outright wrong here.
        if result.up_to_date {
            info_println!(output, "Already up to date.");
        } else if result.would_conflict {
            info_println!(
                output,
                "Would conflict in: {}\n(dry run: nothing was written)",
                result.conflicted_paths.join(", ")
            );
        } else if result.strategy == "fast-forward" {
            info_println!(output, "Would fast-forward\n(dry run: nothing was written)");
        } else {
            info_println!(
                output,
                "Would merge cleanly by the '{}' strategy.\n(dry run: nothing was written)",
                selected_strategy
            );
        }
        return Ok(());
    }

    if result.up_to_date {
        info_println!(output, "Already up to date.");
    } else if result.aborted {
        info_println!(output, "Merge aborted.");
    } else if result.strategy == "quit" {
        info_println!(
            output,
            "Merge state cleared; index and working tree were left unchanged."
        );
    } else if result.continued {
        info_println!(output, "Merge completed.");
    } else if !result.conflicted_paths.is_empty() {
        for report in &result.conflict_kinds {
            if report.kind != "content" {
                continue;
            }
            info_println!(output, "Auto-merging {}", report.path);
            info_println!(
                output,
                "CONFLICT (content): Merge conflict in {}",
                report.path
            );
        }
        info_println!(
            output,
            "Automatic merge failed; fix conflicts and then commit the result."
        );
        if result.autostash.is_some() {
            info_println!(
                output,
                "When finished, apply stashed changes with `libra stash pop`"
            );
        }
    } else if selected_strategy == "in-index" {
        info_println!(output, "In-index merge");
    } else {
        match result.strategy.as_str() {
            "three-way" => info_println!(
                output,
                "Merge made by the '{}' strategy.",
                selected_strategy
            ),
            "octopus" => info_println!(output, "Merge made by the 'octopus' strategy."),
            "ours" => info_println!(output, "Merge made by the 'ours' strategy."),
            "squash" => info_println!(output, "Squash commit -- not updating HEAD"),
            "no-commit" => info_println!(
                output,
                "Automatic merge went well; stopped before committing as requested\n\
                 finalize with 'libra merge --continue'"
            ),
            _ => info_println!(output, "Fast-forward"),
        }
    }
    Ok(())
}

pub(crate) fn merge_error_to_cli(error: MergeError) -> CliError {
    CliError::from(error)
}

pub(crate) fn merge_commit_parents(state: &MergeState) -> Result<Vec<ObjectHash>, String> {
    let mut parents = vec![
        object_hash_from_state("orig_head", &state.orig_head).map_err(|error| error.to_string())?,
    ];
    let targets: Vec<&str> = if state.targets.is_empty() {
        vec![state.target.as_str()]
    } else {
        state.targets.iter().map(String::as_str).collect()
    };
    for target in targets {
        parents.push(object_hash_from_state("target", target).map_err(|error| error.to_string())?);
    }
    Ok(parents)
}

pub(crate) fn merge_commit_message(state: &MergeState) -> String {
    state
        .message
        .clone()
        .unwrap_or_else(|| format!("Merge {} into {}", state.target_ref, state.head_name))
}

pub(crate) fn load_squash_message() -> Result<Option<String>, String> {
    let path = squash_message_path();
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(&path)
        .map(Some)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

pub(crate) fn clear_squash_message() -> Result<(), String> {
    let path = squash_message_path();
    if !path.exists() {
        return Ok(());
    }
    crate::utils::atomic_write::remove_durably(&path)
        .map_err(|error| format!("failed to remove {}: {error}", path.display()))
}

pub(crate) fn record_squash_message(message: &str) -> Result<(), PullMergeError> {
    let path = squash_message_path();
    let body = if message.starts_with("Squashed commit of the following:") {
        message.to_string()
    } else {
        format!("Squashed commit of the following:\n\n{message}")
    };
    write_merge_message(&path, &body)
}

pub(crate) fn append_conflict_comments(message: &str, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return message.to_string();
    }
    let mut out = message.trim_end().to_string();
    out.push_str("\n\n# Conflicts:\n");
    for path in paths {
        out.push_str("#\t");
        out.push_str(&path.display().to_string());
        out.push('\n');
    }
    out
}
