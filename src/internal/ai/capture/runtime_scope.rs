//! Opaque process-local capability selected by the hook runtime.
//!
//! This deliberately lives outside ingress: ingress carries the capability
//! but never imports filesystem APIs or resolves paths itself.

use std::path::PathBuf;

/// Paths bound to the active worktree before a capture command leaves the
/// trusted hook runtime. This type has no `Debug` or serialization surface so
/// provider input can never turn it into a durable payload.
#[derive(Clone)]
pub(crate) struct CaptureRuntimeScope {
    pub(crate) storage_path: PathBuf,
    pub(crate) worktree_root: PathBuf,
}

impl CaptureRuntimeScope {
    pub(crate) fn new(storage_path: PathBuf, worktree_root: PathBuf) -> Self {
        Self {
            storage_path,
            worktree_root,
        }
    }
}
