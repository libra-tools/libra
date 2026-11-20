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
    internal::{
        index::Index,
        object::{
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
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
const MAX_SHOW_FILES: usize = 10_000;
/// A cursor is an offset into a bounded page, not a request to walk the
/// repository until that index. Larger values are rejected before history or
/// status enumeration.
const MAX_CURSOR_OFFSET: usize = 100_000;

/// Read `vcs.identity` for the repository this bridge process was started in.
///
/// The response carries only non-path identity: the absolute worktree path is
/// never returned to the peer, because this method is model-facing and the
/// Host binds a session to a repository from its own trusted discovery, not
/// from a value a model could read. The path hash and `worktree_id` are the
/// stable identity.
pub async fn identity() -> Result<Value, BridgeError> {
    let workdir = pinned_workdir(discovered_workdir()?)?;
    let canonical = workdir;
    let head = Head::current_result()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.identity: read HEAD", error))?;
    let commit = Head::current_commit_result()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.identity: resolve HEAD", error))?;
    let (branch, unborn, head_oid) = match head {
        Head::Branch(name) => {
            let oid = commit.as_ref().map(ToString::to_string);
            (Some(name), oid.is_none(), oid)
        }
        Head::Detached(oid) => (None, false, Some(oid.to_string())),
    };
    Ok(json!({
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
    let limit = page_limit(params)?;
    let mut entries = status_entries(&workdir).await?;
    if entries.len() > MAX_SHOW_FILES {
        return Err(BridgeError::invalid_params(
            "vcs.status refuses a worktree with more than 10000 status entries",
        ));
    }
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
    for path in &paths {
        // Refuse symlink ancestors before the diff service can follow them.
        // The spelling is otherwise unchanged.
        vcs::canonicalize_existing_spelling(path)?;
    }
    let limit = page_limit(params)?;
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
    let limit = page_limit(params)?;
    let Some(head) = Head::current_commit_result()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.log: resolve HEAD", error))?
    else {
        return Ok(json!({
            "commits": [],
            "cursor": { "next": Value::Null },
        }));
    };
    let depth = offset.saturating_add(limit).saturating_add(1);
    let commits = get_reachable_commits(head.to_string(), Some(depth))
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.log", error))?;
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
    let mut files = if let Some(parent_oid) = parent {
        let mut args = DiffArgs::for_service(false, Some(parent_oid.to_string()), Vec::new());
        args.new = Some(oid.to_string());
        match run_diff_for_service(args).await {
            Ok(output) => {
                let mut files = Vec::new();
                for file in output.files {
                    files.push(json!({
                        "path": file.path,
                        "state": if file.rename_from.is_some() { "rename" } else { "staged" },
                        "oldPath": file.rename_from,
                    }));
                    if files.len() > MAX_SHOW_FILES {
                        return Err(BridgeError::invalid_params(
                            "vcs.show refuses a commit with more than 10000 files",
                        ));
                    }
                }
                files
            }
            Err(error) => {
                return Err(match vcs::sanitize_diff_failure(&error) {
                    vcs::SanitizedDiffFailure::NotInRepo => {
                        BridgeError::scope_mismatch("vcs.show has no working repository in scope")
                    }
                    vcs::SanitizedDiffFailure::SymlinkParent => BridgeError::invalid_params(
                        "vcs.show refuses a path whose parent is a symlink or not a directory",
                    ),
                    vcs::SanitizedDiffFailure::Other(text) => BridgeError::internal(format!(
                        "vcs.show could not diff '{oid}' against its parent ({text})"
                    )),
                });
            }
        }
    } else {
        let mut files = Vec::new();
        root_commit_files(commit.tree_id, "", &mut files)?;
        files
    };
    if files.len() > MAX_SHOW_FILES {
        return Err(BridgeError::invalid_params(
            "vcs.show refuses a commit with more than 10000 files",
        ));
    }
    let offset = cursor_offset(params)?;
    let limit = page_limit(params)?;
    let next = if files.len() > offset.saturating_add(limit) {
        Some((offset + limit).to_string())
    } else {
        None
    };
    files = files.into_iter().skip(offset).take(limit).collect();
    Ok(json!({
        "oid": oid.to_string(),
        "summary": summary_line(&commit.message),
        "parents": commit.parent_commit_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "files": files,
        "cursor": { "next": next },
    }))
}

fn root_commit_files(
    tree_id: ObjectHash,
    prefix: &str,
    files: &mut Vec<Value>,
) -> Result<(), BridgeError> {
    let tree = load_object::<Tree>(&tree_id)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.show: read tree", error))?;
    let mut items = tree.tree_items;
    items.sort_by(|left, right| left.name.cmp(&right.name));
    for item in items {
        let path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{prefix}/{}", item.name)
        };
        if item.mode == TreeItemMode::Tree {
            root_commit_files(item.id, &path, files)?;
        } else {
            files.push(json!({
                "path": path,
                "state": "staged",
                "oldPath": Value::Null,
            }));
        }
        if files.len() > MAX_SHOW_FILES {
            return Err(BridgeError::invalid_params(
                "vcs.show refuses a commit with more than 10000 files",
            ));
        }
    }
    Ok(())
}

/// Keep the discovered worktree path. `canonicalize` would follow a symlink
/// that replaced the directory and report another tree as this repository.
fn pinned_workdir(workdir: PathBuf) -> Result<PathBuf, BridgeError> {
    let metadata = std::fs::symlink_metadata(&workdir).map_err(|error| {
        BridgeError::scope_mismatch(format!(
            "not_a_repository: vcs.identity could not inspect the worktree ({})",
            vcs::withheld_detail(&error)
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(BridgeError::scope_mismatch(
            "not_a_repository: vcs.identity refuses a worktree path that is not a real directory",
        ));
    }
    let root = crate::utils::beneath::open_root(&workdir).map_err(|error| {
        BridgeError::scope_mismatch(format!(
            "not_a_repository: vcs.identity could not pin the worktree ({})",
            vcs::withheld_detail(&error)
        ))
    })?;
    confirm_pinned_workdir(&workdir, &root)?;
    Ok(workdir)
}

fn confirm_pinned_workdir(workdir: &Path, held: &std::fs::File) -> Result<(), BridgeError> {
    let original = crate::utils::beneath::identity_for_file(held).map_err(|error| {
        vcs::internal_hiding_host_paths("vcs.identity: identify worktree", error)
    })?;
    let reopened = crate::utils::beneath::open_root(workdir).map_err(|_| {
        BridgeError::scope_mismatch(
            "not_a_repository: vcs.identity worktree changed while it was pinned",
        )
    })?;
    let current = crate::utils::beneath::identity_for_file(&reopened).map_err(|error| {
        vcs::internal_hiding_host_paths("vcs.identity: reidentify worktree", error)
    })?;
    if original != current {
        return Err(BridgeError::scope_mismatch(
            "not_a_repository: vcs.identity worktree changed while it was pinned",
        ));
    }
    Ok(())
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
    let index_path = path::try_index()
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.status: locate index", error))?;
    let index = Index::load(&index_path)
        .map_err(|error| vcs::internal_hiding_host_paths("vcs.status: read index", error))?;
    for stage in [1_u8, 2, 3] {
        for entry in index.tracked_entries(stage) {
            conflicts.insert(rel_path(workdir, Path::new(&entry.name))?);
        }
    }
    let staged = changes_to_be_committed_safe().await.map_err(status_error)?;
    let unstaged = changes_to_be_staged().map_err(status_error)?;
    let mut entries = Vec::new();
    for path in &conflicts {
        entries.push(entry(path, "conflict", None));
    }
    push_changes(&mut entries, workdir, &staged, "staged", &conflicts)?;
    push_changes(&mut entries, workdir, &unstaged, "unstaged", &conflicts)?;
    Ok(entries)
}

fn push_changes(
    entries: &mut Vec<Value>,
    workdir: &Path,
    changes: &crate::command::status::Changes,
    state: &str,
    conflicts: &BTreeSet<String>,
) -> Result<(), BridgeError> {
    let relative = changes.to_relative();
    for path in relative.modified.iter().chain(relative.deleted.iter()) {
        let text = rel_path(workdir, path)?;
        if conflicts.contains(&text) {
            continue;
        }
        entries.push(entry(&text, state, None));
    }
    for path in &relative.new {
        let text = rel_path(workdir, path)?;
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
        let text = rel_path(workdir, new)?;
        if conflicts.contains(&text) {
            continue;
        }
        entries.push(entry(&text, "rename", Some(rel_path(workdir, old)?)));
    }
    Ok(())
}

fn entry(path: &str, state: &str, old: Option<String>) -> Value {
    json!({
        "path": path,
        "state": state,
        "oldPath": old,
    })
}

fn rel_path(workdir: &Path, path: &Path) -> Result<String, BridgeError> {
    let relative = if path.is_absolute() {
        path.strip_prefix(workdir)
            .map_err(|_| BridgeError::internal("vcs.status returned a path outside the worktree"))?
    } else {
        path
    };
    let text = relative
        .to_str()
        .ok_or_else(|| BridgeError::internal("vcs.status returned a path that is not UTF-8"))?
        .replace('\\', "/");
    if text.is_empty()
        || text.starts_with('/')
        || text.contains(':')
        || text
            .split('/')
            .any(|component| component == ".." || component.is_empty())
    {
        return Err(BridgeError::internal("vcs.status returned an unsafe path"));
    }
    Ok(text)
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
    let Some(value) = params.as_ref().and_then(|value| value.get("cursor")) else {
        return Ok(0);
    };
    if value.is_null() {
        return Ok(0);
    }
    let text = value
        .as_str()
        .ok_or_else(|| BridgeError::invalid_params("cursor must be a string or null"))?;
    if text.is_empty() {
        return Ok(0);
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(BridgeError::invalid_params(
            "cursor must be an opaque decimal offset",
        ));
    }
    let offset = text
        .parse::<usize>()
        .map_err(|_| BridgeError::invalid_params("cursor must be an opaque decimal offset"))?;
    if offset > MAX_CURSOR_OFFSET {
        return Err(BridgeError::invalid_params(format!(
            "cursor exceeds the {MAX_CURSOR_OFFSET}-entry offset cap"
        )));
    }
    Ok(offset)
}

fn page_limit(params: &Option<Value>) -> Result<usize, BridgeError> {
    let Some(value) = params.as_ref().and_then(|value| value.get("limit")) else {
        return Ok(20);
    };
    let requested = value
        .as_u64()
        .ok_or_else(|| BridgeError::invalid_params("limit must be a non-negative integer"))?;
    Ok(requested.clamp(1, MAX_PAGE as u64) as usize)
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
        BridgeError::scope_mismatch("not_a_repository: vcs.status cannot access the worktree")
    } else {
        vcs::internal_hiding_host_paths("vcs.status", text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_offset_rejects_an_unbounded_walk() {
        assert_eq!(cursor_offset(&None).expect("default"), 0);
        assert_eq!(
            cursor_offset(&Some(json!({"cursor": "100000"}))).expect("cap"),
            MAX_CURSOR_OFFSET
        );
        let error = cursor_offset(&Some(json!({"cursor": "100001"}))).expect_err("over cap");
        assert!(error.message.contains("offset cap"), "{}", error.message);
        assert!(cursor_offset(&Some(json!({"cursor": "nope"}))).is_err());
        for value in [json!(4), json!(false), json!([]), json!({})] {
            assert!(cursor_offset(&Some(json!({"cursor": value}))).is_err());
        }
        assert_eq!(cursor_offset(&Some(json!({"cursor": null}))).unwrap(), 0);
        assert_eq!(page_limit(&None).unwrap(), 20);
        assert_eq!(
            page_limit(&Some(json!({"limit": u64::MAX}))).unwrap(),
            MAX_PAGE
        );
        for value in [json!(-1), json!("10"), json!(null), json!(1.5)] {
            assert!(page_limit(&Some(json!({"limit": value}))).is_err());
        }
    }

    #[test]
    fn status_rejects_paths_outside_the_worktree() {
        let root = std::env::temp_dir().join("status-root");
        assert_eq!(
            rel_path(&root, &root.join("nested/file")).unwrap(),
            "nested/file"
        );
        assert_eq!(
            rel_path(&root, Path::new("nested/file")).unwrap(),
            "nested/file"
        );
        assert!(rel_path(&root, &root.with_file_name("other").join("secret")).is_err());
        assert!(rel_path(&root, Path::new("../secret")).is_err());
        assert!(rel_path(&root, Path::new("C:\\secret")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn identity_workdir_does_not_follow_a_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("real directory");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let error = pinned_workdir(link).expect_err("symlink worktree");
        assert!(
            error.message.contains("real directory"),
            "{}",
            error.message
        );
        assert_eq!(pinned_workdir(real.clone()).expect("real worktree"), real);
    }

    #[cfg(unix)]
    #[test]
    fn identity_detects_worktree_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workdir = dir.path().join("repo");
        std::fs::create_dir(&workdir).expect("worktree");
        let held = crate::utils::beneath::open_root(&workdir).expect("pin");
        std::fs::rename(&workdir, dir.path().join("original")).expect("move original");
        std::fs::create_dir(&workdir).expect("replace worktree");
        let error = confirm_pinned_workdir(&workdir, &held).expect_err("replacement must fail");
        assert!(error.message.contains("changed"), "{}", error.message);
    }
}
