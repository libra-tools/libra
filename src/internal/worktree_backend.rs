//! Backend-neutral contracts for mounted Libra worktrees.
//!
//! Libra owns Git semantics and durable desired state. A backend driver only
//! prepares a POSIX-visible worktree, reports health and optional changed-path
//! candidates, flushes backend data when required, and tears the worktree down.
//! The contract deliberately does not mirror POSIX operations: filesystem
//! crates already provide those APIs, while build tools consume their mounts.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const BACKEND_CONTROL_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Local,
    ScorpioFs,
    BrewFs,
}

impl BackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::ScorpioFs => "scorpiofs",
            Self::BrewFs => "brewfs",
        }
    }
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub posix_mount: bool,
    pub revision_projection: bool,
    pub native_change_detection: bool,
    pub persistent_volume: bool,
    pub multi_client: bool,
    pub flush_before_commit: bool,
}

impl BackendCapabilities {
    pub const fn local() -> Self {
        Self {
            posix_mount: true,
            revision_projection: false,
            native_change_detection: false,
            persistent_volume: false,
            multi_client: false,
            flush_before_commit: false,
        }
    }

    pub const fn scorpiofs() -> Self {
        Self {
            posix_mount: true,
            revision_projection: true,
            native_change_detection: true,
            persistent_volume: false,
            multi_client: false,
            flush_before_commit: false,
        }
    }

    pub const fn brewfs() -> Self {
        Self {
            posix_mount: true,
            revision_projection: false,
            native_change_detection: false,
            persistent_volume: true,
            multi_client: true,
            flush_before_commit: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendDescriptor {
    pub kind: BackendKind,
    pub display_name: &'static str,
    pub protocol_version: u32,
    pub capabilities: BackendCapabilities,
    pub available: bool,
    pub unavailable_reason: Option<&'static str>,
}

impl BackendDescriptor {
    pub const fn local() -> Self {
        Self {
            kind: BackendKind::Local,
            display_name: "Local filesystem",
            protocol_version: BACKEND_CONTROL_PROTOCOL_VERSION,
            capabilities: BackendCapabilities::local(),
            available: true,
            unavailable_reason: None,
        }
    }

    pub const fn scorpiofs(available: bool) -> Self {
        Self {
            kind: BackendKind::ScorpioFs,
            display_name: "ScorpioFS remote projection",
            protocol_version: BACKEND_CONTROL_PROTOCOL_VERSION,
            capabilities: BackendCapabilities::scorpiofs(),
            available,
            unavailable_reason: if available {
                None
            } else {
                Some("requires Linux and the scorpiofs-direct feature")
            },
        }
    }

    pub const fn brewfs(available: bool) -> Self {
        Self {
            kind: BackendKind::BrewFs,
            display_name: "BrewFS persistent volume",
            protocol_version: BACKEND_CONTROL_PROTOCOL_VERSION,
            capabilities: BackendCapabilities::brewfs(),
            available,
            unavailable_reason: if available {
                None
            } else {
                Some("requires a BrewFS SDK runtime implementation")
            },
        }
    }
}

pub struct BackendRegistry;

impl BackendRegistry {
    pub fn builtins() -> Vec<BackendDescriptor> {
        vec![
            BackendDescriptor::local(),
            BackendDescriptor::scorpiofs(cfg!(all(
                target_os = "linux",
                feature = "scorpiofs-direct"
            ))),
            // The driver boundary is implemented, but BrewFS 0.1.2 does not
            // yet export the complete mount constructor used by its binary.
            BackendDescriptor::brewfs(false),
        ]
    }

    pub fn descriptor(kind: BackendKind) -> Option<BackendDescriptor> {
        Self::builtins()
            .into_iter()
            .find(|descriptor| descriptor.kind == kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendMountSource {
    LocalDirectory {
        path: PathBuf,
    },
    RemoteProjection {
        remote_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_oid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change_layer: Option<String>,
    },
    PersistentVolume {
        volume: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendMountRequest {
    pub instance_id: String,
    pub worktree_id: String,
    pub source: BackendMountSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mountpoint_hint: Option<PathBuf>,
    #[serde(default = "default_ready_timeout_secs")]
    pub ready_timeout_secs: u64,
}

fn default_ready_timeout_secs() -> u64 {
    120
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendMountSession {
    pub backend: BackendKind,
    pub session_id: String,
    pub mountpoint: PathBuf,
    pub cleanup_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_oid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendHealth {
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendLifecycle {
    Detached,
    Mounting,
    Ready,
    SwitchingBase,
    Unmounting,
    RecoverableError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    ModeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedPath {
    pub kind: ChangeKind,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

impl ChangedPath {
    pub fn validate(&self) -> Result<(), WorktreeBackendError> {
        validate_relative_path(&self.path)?;
        if let Some(source_path) = self.source_path.as_deref() {
            validate_relative_path(source_path)?;
        }
        if matches!(self.kind, ChangeKind::Renamed) && self.source_path.is_none() {
            return Err(WorktreeBackendError::InvalidChangedPath(
                "a renamed path must include source_path".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub mount_id: String,
    pub generation: u64,
    #[serde(default)]
    pub changes: Vec<ChangedPath>,
}

impl ChangeSet {
    pub fn validate(&self) -> Result<(), WorktreeBackendError> {
        if self.mount_id.trim().is_empty() {
            return Err(WorktreeBackendError::InvalidRequest(
                "change set mount_id cannot be empty".to_string(),
            ));
        }
        for change in &self.changes {
            change.validate()?;
        }
        Ok(())
    }

    pub fn candidate_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(self.changes.len() * 2);
        for change in &self.changes {
            paths.push(PathBuf::from(&change.path));
            if let Some(source_path) = change.source_path.as_deref() {
                paths.push(PathBuf::from(source_path));
            }
        }
        paths.sort();
        paths.dedup();
        paths
    }
}

#[derive(Debug, Error)]
pub enum WorktreeBackendError {
    #[error("invalid worktree backend request: {0}")]
    InvalidRequest(String),
    #[error("backend '{backend}' does not support source type '{source}'")]
    UnsupportedSource {
        backend: BackendKind,
        source: &'static str,
    },
    #[error("worktree backend '{backend}' is unavailable: {reason}")]
    Unavailable {
        backend: BackendKind,
        reason: String,
    },
    #[error("worktree backend '{backend}' operation '{operation}' failed: {source}")]
    Operation {
        backend: BackendKind,
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("invalid backend changed path: {0}")]
    InvalidChangedPath(String),
}

impl WorktreeBackendError {
    pub fn operation(
        backend: BackendKind,
        operation: &'static str,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self::Operation {
            backend,
            operation,
            source: source.into(),
        }
    }
}

#[async_trait]
pub trait WorktreeBackendDriver: Send + Sync {
    fn descriptor(&self) -> BackendDescriptor;

    async fn mount(
        &self,
        request: &BackendMountRequest,
    ) -> Result<BackendMountSession, WorktreeBackendError>;

    async fn health(
        &self,
        session: &BackendMountSession,
    ) -> Result<BackendHealth, WorktreeBackendError>;

    async fn changed_paths(
        &self,
        _session: &BackendMountSession,
    ) -> Result<Option<ChangeSet>, WorktreeBackendError> {
        Ok(None)
    }

    async fn flush(
        &self,
        _session: &BackendMountSession,
    ) -> Result<(), WorktreeBackendError> {
        Ok(())
    }

    async fn unmount(
        &self,
        session: &BackendMountSession,
    ) -> Result<(), WorktreeBackendError>;

    async fn recover(
        &self,
        request: &BackendMountRequest,
    ) -> Result<BackendMountSession, WorktreeBackendError> {
        self.mount(request).await
    }
}

fn validate_relative_path(path: &str) -> Result<(), WorktreeBackendError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\0') {
        return Err(WorktreeBackendError::InvalidChangedPath(
            "path must be a non-empty relative path".to_string(),
        ));
    }
    if path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err(WorktreeBackendError::InvalidChangedPath(
            "path must be normalized and must not contain traversal".to_string(),
        ));
    }
    Ok(())
}
