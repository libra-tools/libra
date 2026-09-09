//! Request-local binding for async operations and explicitly propagated workers.
//!
//! Each operation future owns a fresh slot, including nested operations and
//! separate branches of `join!`. A synchronous override only changes that slot.
//! Bare sibling futures sharing one slot must establish their own operation
//! boundaries before holding overrides across awaits. Spawned tasks and threads
//! do not inherit Tokio task locals: capture the value and rebind explicitly.

use std::{
    future::Future,
    sync::{Arc, RwLock},
};

use super::RequestScope;

type ScopeSlot = Arc<RwLock<Option<RequestScope>>>;

// Standalone CLI dispatch is serialized and retains its synchronous fallback.
static REQUEST_SCOPE: RwLock<Option<RequestScope>> = RwLock::new(None);

tokio::task_local! {
    static TASK_SCOPE: ScopeSlot;
}

enum TargetSlot {
    Global,
    Task(ScopeSlot),
}

impl TargetSlot {
    fn value(&self) -> &RwLock<Option<RequestScope>> {
        match self {
            Self::Global => &REQUEST_SCOPE,
            Self::Task(slot) => slot,
        }
    }
}

/// Restores the exact slot overridden at creation, even when dropped elsewhere.
#[must_use = "the override ends when this guard is dropped"]
pub struct ScopeOverrideGuard {
    target: TargetSlot,
    previous: Option<RequestScope>,
}

impl Drop for ScopeOverrideGuard {
    fn drop(&mut self) {
        let _ = replace_value(self.target.value(), self.previous.take());
    }
}

fn read_value<T>(
    slot: &RwLock<Option<RequestScope>>,
    project: &impl Fn(&Option<RequestScope>) -> T,
) -> T {
    match slot.read() {
        Ok(value) => project(&value),
        Err(poison) => project(&poison.into_inner()),
    }
}

fn replace_value(
    slot: &RwLock<Option<RequestScope>>,
    next: Option<RequestScope>,
) -> Option<RequestScope> {
    // A poisoned lock still holds one complete Option: replacement never awaits.
    match slot.write() {
        Ok(mut value) => std::mem::replace(&mut *value, next),
        Err(poison) => std::mem::replace(&mut *poison.into_inner(), next),
    }
}

/// Project only owned fields under the lock; callers perform fallback and I/O afterwards.
pub(super) fn with_current<T>(project: impl Fn(&Option<RequestScope>) -> T) -> T {
    match TASK_SCOPE.try_with(|slot| read_value(slot, &project)) {
        // Explicit None means unpinned in THIS request, not global fallback.
        Ok(value) => value,
        Err(_) => read_value(&REQUEST_SCOPE, &project),
    }
}

pub(super) fn replace(next: Option<RequestScope>) -> ScopeOverrideGuard {
    let target = match TASK_SCOPE.try_with(Arc::clone) {
        Ok(slot) => TargetSlot::Task(slot),
        Err(_) => TargetSlot::Global,
    };
    let previous = replace_value(target.value(), next);
    ScopeOverrideGuard { target, previous }
}

/// Bind an already-resolved value for every poll and cancellation of a future.
pub(crate) async fn with_request_scope<T>(
    scope: Option<RequestScope>,
    future: impl Future<Output = T>,
) -> T {
    TASK_SCOPE.scope(Arc::new(RwLock::new(scope)), future).await
}

/// Rebind a captured value on a blocking worker without sharing mutable slots.
pub(crate) fn with_request_scope_sync<T>(
    scope: Option<RequestScope>,
    operation: impl FnOnce() -> T,
) -> T {
    TASK_SCOPE.sync_scope(Arc::new(RwLock::new(scope)), operation)
}
