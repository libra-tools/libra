//! Request-local binding for async operations and explicitly propagated workers.
//!
//! Each operation future owns a fresh slot, including nested operations and
//! separate branches of `join!`. A synchronous override only changes that slot.
//! Bare sibling futures sharing one slot must establish their own operation
//! boundaries before holding overrides across awaits. Spawned tasks and threads
//! do not inherit Tokio task locals: capture the value and rebind explicitly.

#[cfg(test)]
use std::cell::RefCell;
use std::{
    future::Future,
    sync::{Arc, RwLock},
};

use super::RequestScope;

type ScopeSlot = Arc<RwLock<Option<RequestScope>>>;

// Standalone CLI dispatch is serialized and retains its synchronous fallback.
//
// Unit tests run many independent synchronous callers in parallel. Give each
// test thread its own fallback slot so one test cannot restore a temporary
// repository scope captured by another after that repository has been
// deleted. Explicit worker propagation continues to use `TASK_SCOPE`.
#[cfg(not(test))]
static REQUEST_SCOPE: RwLock<Option<RequestScope>> = RwLock::new(None);

#[cfg(test)]
thread_local! {
    static REQUEST_SCOPE: RefCell<Option<ScopeSlot>> = const { RefCell::new(None) };
}

tokio::task_local! {
    static TASK_SCOPE: ScopeSlot;
}

enum TargetSlot {
    #[cfg(not(test))]
    Global,
    #[cfg(test)]
    Global(ScopeSlot),
    Task(ScopeSlot),
}

impl TargetSlot {
    fn value(&self) -> &RwLock<Option<RequestScope>> {
        match self {
            #[cfg(not(test))]
            Self::Global => &REQUEST_SCOPE,
            #[cfg(test)]
            Self::Global(slot) => slot,
            Self::Task(slot) => slot,
        }
    }
}

#[cfg(test)]
fn test_request_scope_slot() -> ScopeSlot {
    REQUEST_SCOPE.with(|slot| {
        let mut slot = slot.borrow_mut();
        Arc::clone(slot.get_or_insert_with(|| Arc::new(RwLock::new(None))))
    })
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
        #[cfg(not(test))]
        Err(_) => read_value(&REQUEST_SCOPE, &project),
        #[cfg(test)]
        Err(_) => {
            let slot = test_request_scope_slot();
            read_value(&slot, &project)
        }
    }
}

pub(super) fn replace(next: Option<RequestScope>) -> ScopeOverrideGuard {
    let target = match TASK_SCOPE.try_with(Arc::clone) {
        Ok(slot) => TargetSlot::Task(slot),
        #[cfg(not(test))]
        Err(_) => TargetSlot::Global,
        #[cfg(test)]
        Err(_) => TargetSlot::Global(test_request_scope_slot()),
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

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Barrier, mpsc},
        thread,
    };

    use super::*;
    use crate::internal::worktree_scope::WorktreeScope;

    fn scope(name: &str) -> RequestScope {
        let root = PathBuf::from(format!("/test/{name}"));
        RequestScope {
            scope: WorktreeScope::Linked(name.to_string()),
            workdir: root.clone(),
            gitdir: root.join(".libra"),
            storage: root.join(".libra"),
            worktree_root: root,
        }
    }

    #[test]
    fn synchronous_fallback_scopes_are_isolated_between_test_threads() {
        let barrier = Arc::new(Barrier::new(2));
        let (sender, receiver) = mpsc::channel();

        let mut workers = Vec::new();
        for name in ["scope-a", "scope-b"] {
            let barrier = Arc::clone(&barrier);
            let sender = sender.clone();
            workers.push(thread::spawn(move || {
                let expected = scope(name);
                let _guard = replace(Some(expected.clone()));
                barrier.wait();
                sender
                    .send((name.to_string(), with_current(Clone::clone)))
                    .expect("test receiver remains available");
            }));
        }
        drop(sender);

        let observed = receiver.into_iter().collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("scope worker should not panic");
        }

        assert_eq!(observed.len(), 2);
        for (name, actual) in observed {
            assert_eq!(actual, Some(scope(&name)));
        }
    }
}
