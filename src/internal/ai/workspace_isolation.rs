//! Isolated workspace types and materialization — plan-20260920 RC-03/RC-07/RC-23.
//!
//! KEEP `review` / `investigate` depend on this module. RC-23 deleted the
//! executor SCC (`orchestrator` / `workspace_snapshot`); this seam now
//! materializes a full copy only. Review/investigate already disable FUSE
//! before calling [`materialize_isolated_workspace`].

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::Result;
use uuid::Uuid;

use crate::internal::ai::agent_run::{AgentRunId, WorkspaceStrategy};

/// Physical materialization backend. Copy-only after RC-23.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskWorkspaceBackend {
    Shared,
    Copy,
    Fuse,
}

impl TaskWorkspaceBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Shared => "shared workspace",
            Self::Copy => "copy worktree",
            Self::Fuse => "FUSE worktree",
        }
    }
}

/// Per-session FUSE-provisioning flag. Review/investigate still construct
/// this and call [`Self::disable_first_time`]; materialization ignores FUSE.
#[derive(Clone, Debug)]
pub struct FuseProvisionState {
    disabled: Arc<AtomicBool>,
}

impl Default for FuseProvisionState {
    fn default() -> Self {
        Self {
            disabled: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl FuseProvisionState {
    pub fn disable_first_time(&self) -> bool {
        self.disabled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Acquire)
    }
}

/// Isolation settings for a review / investigate / sub-agent workspace.
#[derive(Clone)]
pub struct WorkspaceIsolationConfig {
    /// Per-session FUSE flag (always treated as disabled after RC-23).
    pub fuse_state: FuseProvisionState,
    /// `.libra/sessions` root the copy is written under.
    pub sessions_root: PathBuf,
    /// Whether an expensive full-copy fallback is permitted.
    pub allow_full_copy: bool,
}

impl std::fmt::Debug for WorkspaceIsolationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceIsolationConfig")
            .field("sessions_root", &self.sessions_root)
            .field("allow_full_copy", &self.allow_full_copy)
            .finish_non_exhaustive()
    }
}

/// Isolated workspace root. Cleanup removes the copy directory.
pub struct SubAgentWorkspace {
    root: PathBuf,
    strategy: WorkspaceStrategy,
}

impl SubAgentWorkspace {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn strategy(&self) -> WorkspaceStrategy {
        self.strategy
    }

    pub fn backend(&self) -> TaskWorkspaceBackend {
        TaskWorkspaceBackend::Copy
    }

    pub fn cleanup(self) -> io::Result<()> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        Ok(())
    }
}

/// Error materializing an isolated workspace.
#[derive(Debug, thiserror::Error)]
pub enum SubAgentWorkspaceError {
    #[error("full-copy workspace materialization is not permitted")]
    Unavailable,
    #[error("failed to materialize isolated workspace: {0}")]
    Io(#[from] io::Error),
}

/// Materialize an isolated workspace for a review / investigate run.
///
/// Isolation failure is terminal: falling back to the main worktree
/// would violate S2-INV-03.
pub fn materialize_isolated_workspace(
    main_working_dir: &Path,
    thread_id: Uuid,
    agent_run_id: AgentRunId,
    isolation: &WorkspaceIsolationConfig,
) -> Result<SubAgentWorkspace, SubAgentWorkspaceError> {
    let _ = isolation.fuse_state.is_disabled();
    let _ = isolation.allow_full_copy;
    let dest = isolation
        .sessions_root
        .join(thread_id.to_string())
        .join(agent_run_id.0.to_string())
        .join("workspace");
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::create_dir_all(&dest)?;
    copy_tree(main_working_dir, &dest, main_working_dir, &dest)?;
    Ok(SubAgentWorkspace {
        root: dest,
        strategy: WorkspaceStrategy::FullCopy,
    })
}

fn copy_tree(src: &Path, dest: &Path, _src_root: &Path, dest_root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".libra" {
            continue;
        }
        let from = entry.path();
        if from.starts_with(dest_root) {
            continue;
        }
        let to = dest.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            fs::create_dir_all(&to)?;
            copy_tree(&from, &to, _src_root, dest_root)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&from)?;
                std::os::unix::fs::symlink(target, &to)?;
            }
            #[cfg(not(unix))]
            {
                if from.is_dir() {
                    fs::create_dir_all(&to)?;
                    copy_tree(&from, &to, _src_root, dest_root)?;
                } else {
                    fs::copy(&from, &to)?;
                }
            }
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
