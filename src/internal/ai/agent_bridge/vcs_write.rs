//! `vcs.stage`, `vcs.unstage`, and `vcs.commit` (plan-20261118 LVC-06).
//!
//! These do not reuse `commit.create`. Each call names literal repository
//! paths, an `operation_id`, and the HEAD/index/worktree fence the caller
//! observed. A stale fence returns before any index or commit write.

use std::{collections::BTreeSet, path::Path};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{protocol::BridgeError, vcs};
use crate::{
    command::{
        add::{AddArgs, execute_safe as add_execute},
        restore::{RestoreArgs, execute_safe as restore_execute},
        status::{changes_to_be_committed_safe, changes_to_be_staged},
    },
    utils::{path, util},
};

const MAX_FENCE_PATHS: usize = 256;
static WRITE_GATE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFence {
    pub head: String,
    pub index_generation: String,
    pub worktree_generation: String,
}

pub async fn current_write_fence() -> Result<WriteFence, BridgeError> {
    let head = vcs::read_fence()
        .await
        .head
        .map(|oid| oid.to_string())
        .unwrap_or_default();
    let index_path = path::try_index().map_err(|_| {
        BridgeError::scope_mismatch("not_a_repository: vcs write methods need a Libra index")
    })?;
    let index_bytes = std::fs::read(&index_path).unwrap_or_default();
    let workdir = util::try_working_dir().map_err(|_| {
        BridgeError::scope_mismatch("not_a_repository: vcs write methods need a Libra worktree")
    })?;
    Ok(WriteFence {
        head,
        index_generation: sha256_hex(&index_bytes),
        worktree_generation: worktree_generation(&workdir).await?,
    })
}

/// Re-read the fence and run `write` while this process holds the write gate.
/// Another process can still change the repository; that residual race is not
/// closed by this gate.
pub async fn run_fenced<T>(
    expected: &WriteFence,
    write: impl std::future::Future<Output = Result<T, BridgeError>>,
) -> Result<T, BridgeError> {
    let _gate = WRITE_GATE.lock().await;
    let actual = current_write_fence().await?;
    require_matching_fence(
        &actual,
        &expected.head,
        &expected.index_generation,
        &expected.worktree_generation,
    )?;
    write.await
}

pub fn require_matching_fence(
    actual: &WriteFence,
    expected_head: &str,
    expected_index: &str,
    expected_worktree: &str,
) -> Result<(), BridgeError> {
    if actual.head == expected_head
        && actual.index_generation == expected_index
        && actual.worktree_generation == expected_worktree
    {
        return Ok(());
    }
    Err(BridgeError::stale_fence(format!(
        "stale_state: expected fence head='{expected_head}' index='{expected_index}' worktree='{expected_worktree}' does not match head='{}' index='{}' worktree='{}'. No index or commit write was performed.",
        actual.head, actual.index_generation, actual.worktree_generation
    )))
}

pub async fn stage_paths(paths: &[String]) -> (Vec<String>, Vec<serde_json::Value>) {
    apply_each(paths, true).await
}

pub async fn unstage_paths(paths: &[String]) -> (Vec<String>, Vec<serde_json::Value>) {
    apply_each(paths, false).await
}

pub async fn commit_index(
    message: &str,
    requested: &[String],
) -> Result<serde_json::Value, BridgeError> {
    let staged = crate::command::status::changes_to_be_committed_safe()
        .await
        .map_err(|error| BridgeError::internal(format!("vcs.commit: read index: {error}")))?;
    let relative = staged.to_relative();
    let mut staged_paths = std::collections::BTreeSet::new();
    for path in relative
        .new
        .iter()
        .chain(relative.modified.iter())
        .chain(relative.deleted.iter())
    {
        staged_paths.insert(path.display().to_string().replace('\\', "/"));
    }
    for (_old, new) in &relative.renamed {
        staged_paths.insert(new.display().to_string().replace('\\', "/"));
    }
    let requested_paths: std::collections::BTreeSet<String> = requested
        .iter()
        .map(|path| path.replace('\\', "/"))
        .collect();
    if staged_paths != requested_paths {
        return Err(BridgeError::invalid_params(format!(
            "vcs.commit paths {requested_paths:?} do not match the staged set {staged_paths:?}; refusing before creating a commit"
        )));
    }
    let created = vcs::commit_create(message, false, false).await?;
    Ok(serde_json::json!({
        "commitOid": created["commit"],
        "paths": staged_paths.into_iter().collect::<Vec<_>>(),
    }))
}

async fn apply_each(paths: &[String], stage: bool) -> (Vec<String>, Vec<serde_json::Value>) {
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    for path in paths {
        let output = quiet_output();
        let result = if stage {
            add_execute(
                AddArgs {
                    pathspec: vec![path.clone()],
                    ..AddArgs::default()
                },
                &output,
            )
            .await
        } else {
            restore_execute(
                RestoreArgs {
                    pathspec: vec![path.clone()],
                    staged: true,
                    source: Some("HEAD".to_string()),
                    ..RestoreArgs::default()
                },
                &output,
            )
            .await
        };
        match result {
            Ok(()) => applied.push(path.clone()),
            Err(error) => failed.push(serde_json::json!({
                "path": path,
                "message": error.to_string(),
            })),
        }
    }
    (applied, failed)
}

fn quiet_output() -> crate::utils::output::OutputConfig {
    let mut output = vcs::service_output();
    output.json_format = None;
    output
}

async fn worktree_generation(workdir: &Path) -> Result<String, BridgeError> {
    let staged = changes_to_be_committed_safe()
        .await
        .map_err(|error| BridgeError::internal(format!("vcs fence: read index: {error}")))?;
    let unstaged = changes_to_be_staged()
        .map_err(|error| BridgeError::internal(format!("vcs fence: read worktree: {error}")))?;
    let mut rows = BTreeSet::new();
    collect_rows(workdir, &staged.to_relative(), "staged", &mut rows);
    collect_rows(workdir, &unstaged.to_relative(), "unstaged", &mut rows);
    if rows.len() > MAX_FENCE_PATHS {
        return Err(BridgeError::invalid_params(format!(
            "vcs fence refuses more than {MAX_FENCE_PATHS} changed paths"
        )));
    }
    Ok(sha256_hex(
        rows.into_iter().collect::<Vec<_>>().join("\n").as_bytes(),
    ))
}

fn collect_rows(
    workdir: &Path,
    changes: &crate::command::status::Changes,
    state: &str,
    rows: &mut BTreeSet<String>,
) {
    for path in changes
        .new
        .iter()
        .chain(changes.modified.iter())
        .chain(changes.deleted.iter())
    {
        let rel = path.display().to_string().replace('\\', "/");
        rows.insert(format!("{state}\t{rel}\t{}", content_digest(workdir, &rel)));
    }
    for (old, new) in &changes.renamed {
        let rel = new.display().to_string().replace('\\', "/");
        let from = old.display().to_string().replace('\\', "/");
        rows.insert(format!(
            "rename\t{from}\t{rel}\t{}",
            content_digest(workdir, &rel)
        ));
    }
}

fn content_digest(workdir: &Path, relative: &str) -> String {
    match std::fs::read(workdir.join(relative)) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => "missing".to_string(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
