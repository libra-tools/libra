//! Proposed read methods `vcs.identity`, `vcs.status`, `vcs.diff`, `vcs.log`,
//! and `vcs.show` (plan-20261118 LVC-02).
//!
//! These are additive. `status.get`, `history.search`, and `diff.get` keep
//! their existing bridge-telemetry and checkpoint meanings. Every value comes
//! from Libra's own discovery, HEAD, status, diff, and commit loaders. The
//! request cannot name an arbitrary revision range, a pathspec, or a
//! repository root.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use git_internal::{
    hash::ObjectHash,
    internal::{index::Index, object::commit::Commit},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    protocol::{BridgeError, MAX_PAGE},
    vcs::{self, DiffMode},
};
use crate::{
    command::{
        diff::{DiffArgs, run_diff_for_service},
        load_object,
        log::get_reachable_commits,
        status::{changes_to_be_committed_safe, changes_to_be_staged},
    },
    internal::head::Head,
    utils::{path, util},
};

const BYTE_CAP: usize = vcs::MAX_DIFF_PATCH_BYTES;

/// Read `vcs.identity` for the repository this bridge process was started in.
///
/// `workdir` is for the Host binding check. Model-facing tools must not
/// forward it. The hash and `worktree_id` are the stable identity.
pub async fn identity() -> Result<Value, BridgeError> {
    let workdir = discovered_workdir()?;
    let canonical = std::fs::canonicalize(&workdir).unwrap_or(workdir);
    let head = Head::current_result().await.map_err(|error| {
        BridgeError::internal(format!("vcs.identity: HEAD is unreadable ({error})"))
    })?;
    let commit = Head::current_commit().await;
    let (branch, unborn, head_oid) = match head {
        Head::Branch(name) => {
            let oid = commit.as_ref().map(ToString::to_string);
            (Some(name), oid.is_none(), oid)
        }
        Head::Detached(oid) => (None, false, Some(oid.to_string())),
    };
    Ok(json!({
        "workdir": canonical.display().to_string(),
        "worktree_root_hash": sha256_hex(canonical.to_string_lossy().as_bytes()),
        "worktree_id": util::worktree_id_for_base(Some(canonical)),
        "head_oid": head_oid,
        "branch": branch,
        "unborn": unborn,
    }))
}

/// Paginated working-tree status. An empty page is returned only after
/// discovery succeeds.
pub async fn status(params: &Option<Value>) -> Result<Value, BridgeError> {
    let workdir = discovered_workdir()?;
    let offset = cursor_offset(params)?;
    let limit = page_limit(params);
    let mut entries = status_entries(&workdir).await?;
    entries.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["path"].as_str().unwrap_or_default())
            .then_with(|| {
                left["state"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["state"].as_str().unwrap_or_default())
            })
    });
    Ok(page_of(entries, offset, limit))
}

/// Bounded diff. `sides` is a closed enum; `oid` is a single commit id.
pub async fn diff(params: &Option<Value>) -> Result<Value, BridgeError> {
    let _workdir = discovered_workdir()?;
    let sides = required_str(params, "sides")?.to_string();
    let path_values = params
        .as_ref()
        .and_then(|value| value.get("paths"))
        .cloned()
        .unwrap_or(json!([]));
    let paths = vcs::parse_diff_params(&Some(json!({
        "mode": "worktree",
        "paths": path_values,
    })))?
    .1;
    let limit = page_limit(params);
    let (mode, against) = match sides.as_str() {
        "worktree-vs-index" => (DiffMode::Worktree, None),
        "index-vs-head" => (DiffMode::Staged, None),
        "worktree-vs-oid" => (DiffMode::Checkpoint, Some(required_oid(params, "oid")?)),
        other => {
            return Err(BridgeError::invalid_params(format!(
                "vcs.diff sides must be worktree-vs-index, index-vs-head, or worktree-vs-oid, got '{other}'"
            )));
        }
    };
    let (data, warnings) = vcs::diff_data(mode, against, paths, limit).await?;
    let mut truncation = if warnings
        .iter()
        .any(|warning| warning["code"] == "diff_truncated")
    {
        "page_cap"
    } else if warnings
        .iter()
        .any(|warning| warning["code"] == "diff_patch_budget_exhausted")
    {
        "byte_cap"
    } else {
        "none"
    };
    let files = data["files"].as_array().cloned().unwrap_or_default();
    let mut projected = Vec::with_capacity(files.len());
    for file in files {
        let binary = file["binary"].as_bool().unwrap_or(false);
        let omitted = file["patch_omitted"].as_bool().unwrap_or(false);
        let file_truncation = if binary {
            "binary"
        } else if omitted {
            "byte_cap"
        } else {
            "none"
        };
        projected.push(json!({
            "path": file["path"],
            "oldPath": file["rename_from"],
            "patch": if binary || omitted { Value::Null } else { file["patch"].clone() },
            "truncated": file_truncation != "none",
            "truncation": file_truncation,
        }));
    }
    if truncation == "none" && projected.iter().any(|file| file["truncation"] == "binary") {
        truncation = "binary";
    }
    Ok(json!({
        "sides": sides,
        "byteCap": BYTE_CAP,
        "truncated": truncation != "none",
        "truncation": truncation,
        "files": projected,
    }))
}

/// Cursor page of commits reachable from HEAD.
pub async fn log(params: &Option<Value>) -> Result<Value, BridgeError> {
    let _workdir = discovered_workdir()?;
    let offset = cursor_offset(params)?;
    let limit = page_limit(params);
    let Some(head) = Head::current_commit().await else {
        return Ok(json!({
            "commits": [],
            "cursor": { "next": Value::Null },
        }));
    };
    let depth = offset.saturating_add(limit).saturating_add(1);
    let commits = get_reachable_commits(head.to_string(), Some(depth))
        .await
        .map_err(|error| BridgeError::internal(format!("vcs.log: {error}")))?;
    let page: Vec<Value> = commits
        .iter()
        .skip(offset)
        .take(limit)
        .map(commit_summary)
        .collect();
    let next = if commits.len() > offset.saturating_add(limit) {
        Some((offset + limit).to_string())
    } else {
        None
    };
    Ok(json!({
        "commits": page,
        "cursor": { "next": next },
    }))
}

/// One commit. `files` compares it with its first parent when one exists.
pub async fn show(params: &Option<Value>) -> Result<Value, BridgeError> {
    let _workdir = discovered_workdir()?;
    let oid = required_oid(params, "oid")?;
    let commit = load_object::<Commit>(&oid)
        .map_err(|_| BridgeError::invalid_params(format!("vcs.show: '{oid}' is not a commit")))?;
    let parent = commit.parent_commit_ids.first().copied();
    let files = if let Some(parent_oid) = parent {
        let mut args = DiffArgs::for_service(false, Some(parent_oid.to_string()), Vec::new());
        args.new = Some(oid.to_string());
        match run_diff_for_service(args).await {
            Ok(output) => output
                .files
                .into_iter()
                .map(|file| {
                    json!({
                        "path": file.path,
                        "state": if file.rename_from.is_some() { "rename" } else { "staged" },
                        "oldPath": file.rename_from,
                    })
                })
                .collect(),
            Err(error) => {
                return Err(BridgeError::internal(format!(
                    "vcs.show could not diff '{oid}' against its parent ({error})"
                )));
            }
        }
    } else {
        Vec::new()
    };
    Ok(json!({
        "oid": oid.to_string(),
        "summary": summary_line(&commit.message),
        "parents": commit.parent_commit_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "files": files,
    }))
}

fn discovered_workdir() -> Result<PathBuf, BridgeError> {
    util::try_working_dir().map_err(|_| {
        BridgeError::scope_mismatch(
            "not_a_repository: vcs.* requires the bridge process to be started inside a Libra worktree",
        )
    })
}

async fn status_entries(workdir: &Path) -> Result<Vec<Value>, BridgeError> {
    let mut conflicts = BTreeSet::new();
    if let Ok(index_path) = path::try_index()
        && let Ok(index) = Index::load(&index_path)
    {
        for stage in [1_u8, 2, 3] {
            for entry in index.tracked_entries(stage) {
                conflicts.insert(rel_path(workdir, Path::new(&entry.name)));
            }
        }
    }
    let staged = changes_to_be_committed_safe().await.map_err(status_error)?;
    let unstaged = changes_to_be_staged().map_err(status_error)?;
    let mut entries = Vec::new();
    for path in &conflicts {
        entries.push(entry(path, "conflict", None));
    }
    push_changes(&mut entries, workdir, &staged, "staged", &conflicts);
    push_changes(&mut entries, workdir, &unstaged, "unstaged", &conflicts);
    Ok(entries)
}

fn push_changes(
    entries: &mut Vec<Value>,
    workdir: &Path,
    changes: &crate::command::status::Changes,
    state: &str,
    conflicts: &BTreeSet<String>,
) {
    let relative = changes.to_relative();
    for path in relative.modified.iter().chain(relative.deleted.iter()) {
        let text = rel_path(workdir, path);
        if conflicts.contains(&text) {
            continue;
        }
        entries.push(entry(&text, state, None));
    }
    for path in &relative.new {
        let text = rel_path(workdir, path);
        if conflicts.contains(&text) {
            continue;
        }
        let kind = if state == "unstaged" {
            "untracked"
        } else {
            "staged"
        };
        entries.push(entry(&text, kind, None));
    }
    for (old, new) in &relative.renamed {
        let text = rel_path(workdir, new);
        if conflicts.contains(&text) {
            continue;
        }
        entries.push(entry(&text, "rename", Some(rel_path(workdir, old))));
    }
}

fn entry(path: &str, state: &str, old: Option<String>) -> Value {
    json!({
        "path": path,
        "state": state,
        "oldPath": old,
    })
}

fn rel_path(workdir: &Path, path: &Path) -> String {
    path.strip_prefix(workdir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn page_of(entries: Vec<Value>, offset: usize, limit: usize) -> Value {
    let next = if entries.len() > offset.saturating_add(limit) {
        Some((offset + limit).to_string())
    } else {
        None
    };
    let page: Vec<Value> = entries.into_iter().skip(offset).take(limit).collect();
    json!({
        "entries": page,
        "cursor": { "next": next },
    })
}

fn commit_summary(commit: &Commit) -> Value {
    json!({
        "oid": commit.id.to_string(),
        "summary": summary_line(&commit.message),
        "authorTime": commit.author.timestamp,
    })
}

fn summary_line(message: &str) -> String {
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn required_oid(params: &Option<Value>, key: &str) -> Result<ObjectHash, BridgeError> {
    let raw = required_str(params, key)?;
    if raw.contains("..") || raw.contains(':') || raw.contains(' ') {
        return Err(BridgeError::invalid_params(
            "vcs.* oid must be one object id, not a revision range or pathspec",
        ));
    }
    crate::internal::object_format::parse_repo_oid(raw)
        .map_err(|_| BridgeError::invalid_params(format!("vcs.* {key} is not an object id")))
}

fn required_str<'a>(params: &'a Option<Value>, key: &str) -> Result<&'a str, BridgeError> {
    params
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::invalid_params(format!("vcs.* requires {key}")))
}

fn cursor_offset(params: &Option<Value>) -> Result<usize, BridgeError> {
    match params
        .as_ref()
        .and_then(|value| value.get("cursor"))
        .and_then(Value::as_str)
    {
        None | Some("") => Ok(0),
        Some(text) => text
            .parse::<usize>()
            .map_err(|_| BridgeError::invalid_params("cursor must be an opaque decimal offset")),
    }
}

fn page_limit(params: &Option<Value>) -> usize {
    params
        .as_ref()
        .and_then(|value| value.get("limit"))
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, MAX_PAGE))
        .unwrap_or(20)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn status_error(error: impl std::fmt::Display) -> BridgeError {
    let text = error.to_string();
    if text.contains("not a libra repository") || text.contains("work tree") {
        BridgeError::scope_mismatch(format!("not_a_repository: {text}"))
    } else {
        BridgeError::internal(format!("vcs.status: {text}"))
    }
}
