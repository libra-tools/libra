//! Process-level structured shutdown owner for `libra code`.
//!
//! Turn-level cooperative cancel and mutation reconciliation stay in
//! [`super::worker`]. This module sequences the remaining process resources
//! (runtime adapter, controller lease, listeners, managed child, control
//! files / locks) under one deadline and one diagnosable result contract.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Canonical resource categories reported when shutdown times out or fails.
pub mod resource {
    pub const RUNTIME_TURN: &str = "runtime_turn";
    pub const MUTATING_RUNTIME_TURN_RECONCILIATION: &str = "mutating_runtime_turn_reconciliation";
    pub const WEB_SERVER: &str = "web_server";
    pub const MCP_SERVER: &str = "mcp_server";
    pub const MANAGED_CODEX_CHILD: &str = "managed_codex_child";
    pub const CONTROLLER_LEASE: &str = "controller_lease";
    pub const CONTROL_LOCK: &str = "control_lock";
    pub const TEMP_FILE: &str = "temp_file";
    pub const PROVIDER_STREAM: &str = "provider_stream";
    pub const FUSE_TASK_WORKTREE: &str = "fuse_task_worktree";
}

/// Result of a process-level structured shutdown.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum LifecycleShutdownError {
    #[error("code lifecycle shutdown timed out waiting for: {unreleased_resources:?}")]
    TimedOut { unreleased_resources: Vec<String> },
    #[error("code lifecycle shutdown failed releasing {failed_resources:?}: {detail}")]
    Failed {
        failed_resources: Vec<String>,
        detail: String,
    },
}

type ShutdownFuture = Pin<Box<dyn Future<Output = Result<(), LifecycleStepError>> + Send>>;

/// One ordered shutdown step owned by [`LifecycleShutdownOwner`].
pub struct LifecycleShutdownStep {
    pub category: String,
    pub run: ShutdownFuture,
}

/// Failure from a single lifecycle step. Timeouts and hard failures both map
/// into the owner's aggregated diagnostic categories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleStepError {
    /// Step hit its (or the owner's) deadline. `categories` overrides the
    /// registered step label when non-empty so nested owners (e.g. AgentRuntime)
    /// can surface precise unreleased classes.
    TimedOut { categories: Vec<String> },
    Failed {
        categories: Vec<String>,
        detail: String,
    },
}

impl LifecycleStepError {
    pub fn timed_out() -> Self {
        Self::TimedOut {
            categories: Vec::new(),
        }
    }

    pub fn timed_out_with(categories: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::TimedOut {
            categories: categories.into_iter().map(Into::into).collect(),
        }
    }

    pub fn failed(detail: impl Into<String>) -> Self {
        Self::Failed {
            categories: Vec::new(),
            detail: detail.into(),
        }
    }

    pub fn failed_with(
        categories: impl IntoIterator<Item = impl Into<String>>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Failed {
            categories: categories.into_iter().map(Into::into).collect(),
            detail: detail.into(),
        }
    }
}

enum OwnerState {
    Idle(Vec<LifecycleShutdownStep>),
    Running,
    Done(Result<(), LifecycleShutdownError>),
}

struct LifecycleShutdownOwnerInner {
    timeout: Duration,
    state: Mutex<OwnerState>,
    /// Terminal result channel. `None` while idle/running; `Some` once Done.
    /// Concurrent waiters subscribe before observing state so they cannot miss
    /// the completion notification.
    result_tx: tokio::sync::watch::Sender<Option<Result<(), LifecycleShutdownError>>>,
}

/// Single owner for SIGINT/SIGTERM, startup-failure cleanup, and normal exit.
///
/// Callers register resources in release order (stop admission / runtime first,
/// then lease, listeners, child processes, finally control lock / temp files).
/// Concurrent `shutdown` calls join the same terminal result.
#[derive(Clone)]
pub struct LifecycleShutdownOwner {
    inner: Arc<LifecycleShutdownOwnerInner>,
}

impl LifecycleShutdownOwner {
    pub fn with_timeout(timeout: Duration) -> Self {
        let (result_tx, _) = tokio::sync::watch::channel(None);
        Self {
            inner: Arc::new(LifecycleShutdownOwnerInner {
                timeout,
                state: Mutex::new(OwnerState::Idle(Vec::new())),
                result_tx,
            }),
        }
    }

    /// Append a shutdown step. Panics in debug if shutdown has already begun;
    /// in release, late steps are dropped so a racing failure path cannot
    /// invent resources after the owner has started draining.
    pub async fn push_step<F>(&self, category: impl Into<String>, run: F)
    where
        F: Future<Output = Result<(), LifecycleStepError>> + Send + 'static,
    {
        let mut state = lock_owner_state(&self.inner.state);
        match &mut *state {
            OwnerState::Idle(steps) => steps.push(LifecycleShutdownStep {
                category: category.into(),
                run: Box::pin(run),
            }),
            OwnerState::Running | OwnerState::Done(_) => {
                debug_assert!(
                    false,
                    "LifecycleShutdownOwner::push_step after shutdown began"
                );
            }
        }
    }

    /// Run every registered step under the owner deadline. Idempotent.
    pub async fn shutdown(&self) -> Result<(), LifecycleShutdownError> {
        let mut result_rx = self.inner.result_tx.subscribe();
        let steps = {
            let mut state = lock_owner_state(&self.inner.state);
            match &mut *state {
                OwnerState::Idle(steps) => {
                    let steps = std::mem::take(steps);
                    *state = OwnerState::Running;
                    Some(steps)
                }
                OwnerState::Running => None,
                OwnerState::Done(result) => return result.clone(),
            }
        };

        if let Some(steps) = steps {
            // If this future is cancelled mid-drain, publish a terminal failure
            // so concurrent waiters cannot block forever on `Running`.
            let cancel_guard = ShutdownCancelGuard {
                inner: Arc::clone(&self.inner),
                completed: false,
            };
            let result = self.run_steps(steps).await;
            cancel_guard.disarm_and_publish(result.clone());
            return result;
        }

        loop {
            if let Some(result) = result_rx.borrow().clone() {
                return result;
            }
            if result_rx.changed().await.is_err() {
                return Err(LifecycleShutdownError::Failed {
                    failed_resources: vec!["lifecycle_owner".to_string()],
                    detail: "lifecycle shutdown result channel closed".to_string(),
                });
            }
        }
    }

    async fn run_steps(
        &self,
        steps: Vec<LifecycleShutdownStep>,
    ) -> Result<(), LifecycleShutdownError> {
        let deadline = Instant::now() + self.inner.timeout;
        let mut timed_out = Vec::new();
        let mut failed = Vec::new();
        let mut failure_detail = String::new();

        for step in steps {
            // Prefer the remaining shared deadline. After it expires, still
            // start each later step (one poll / yield) so stop signals fire,
            // but never extend wall-clock wait past the owner timeout.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let step_result = if remaining.is_zero() {
                let join = tokio::spawn(step.run);
                tokio::task::yield_now().await;
                if join.is_finished() {
                    match join.await {
                        Ok(result) => Ok(result),
                        Err(_) => Err(()),
                    }
                } else {
                    join.abort();
                    let _ = join.await;
                    Err(())
                }
            } else {
                match tokio::time::timeout(remaining, step.run).await {
                    Ok(result) => Ok(result),
                    Err(_) => Err(()),
                }
            };

            match step_result {
                Ok(Ok(())) => {}
                Ok(Err(LifecycleStepError::TimedOut { categories })) => {
                    if categories.is_empty() {
                        timed_out.push(step.category);
                    } else {
                        for category in categories {
                            if !timed_out.contains(&category) {
                                timed_out.push(category);
                            }
                        }
                    }
                }
                Ok(Err(LifecycleStepError::Failed { categories, detail })) => {
                    if failure_detail.is_empty() {
                        failure_detail = detail;
                    }
                    if categories.is_empty() {
                        failed.push(step.category);
                    } else {
                        for category in categories {
                            if !failed.contains(&category) {
                                failed.push(category);
                            }
                        }
                    }
                }
                Err(()) => {
                    timed_out.push(step.category);
                }
            }
        }

        if !timed_out.is_empty() {
            // Prefer timeout diagnostics when any step hit the shared deadline;
            // include hard failures so operators still see every stuck class.
            let mut unreleased = timed_out;
            for category in failed {
                if !unreleased.contains(&category) {
                    unreleased.push(category);
                }
            }
            return Err(LifecycleShutdownError::TimedOut {
                unreleased_resources: unreleased,
            });
        }

        if !failed.is_empty() {
            return Err(LifecycleShutdownError::Failed {
                failed_resources: failed,
                detail: failure_detail,
            });
        }

        Ok(())
    }
}

struct ShutdownCancelGuard {
    inner: Arc<LifecycleShutdownOwnerInner>,
    completed: bool,
}

fn lock_owner_state(state: &Mutex<OwnerState>) -> std::sync::MutexGuard<'_, OwnerState> {
    // std Mutex so Drop can always publish; recover from poison rather than panic.
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn publish_terminal_result(
    inner: &LifecycleShutdownOwnerInner,
    result: Result<(), LifecycleShutdownError>,
) {
    {
        let mut state = lock_owner_state(&inner.state);
        *state = OwnerState::Done(result.clone());
    }
    let _ = inner.result_tx.send(Some(result));
}

impl ShutdownCancelGuard {
    fn disarm_and_publish(mut self, result: Result<(), LifecycleShutdownError>) {
        publish_terminal_result(&self.inner, result);
        self.completed = true;
    }
}

impl Drop for ShutdownCancelGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let result = Err(LifecycleShutdownError::Failed {
            failed_resources: vec!["lifecycle_owner".to_string()],
            detail: "lifecycle shutdown was cancelled before it published a terminal result"
                .to_string(),
        });
        // Always publish via std Mutex — tokio try_lock could miss and leave
        // concurrent waiters stuck in `Running` forever.
        {
            let mut state = lock_owner_state(&self.inner.state);
            if !matches!(*state, OwnerState::Running) {
                return;
            }
            *state = OwnerState::Done(result.clone());
        }
        let _ = self.inner.result_tx.send(Some(result));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_is_idempotent_and_aggregates_timeout_categories() {
        let owner = LifecycleShutdownOwner::with_timeout(Duration::from_millis(30));
        owner
            .push_step(resource::RUNTIME_TURN, async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok(())
            })
            .await;
        owner
            .push_step(resource::WEB_SERVER, async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(())
            })
            .await;
        owner
            .push_step(resource::MCP_SERVER, async {
                Err(LifecycleStepError::timed_out())
            })
            .await;

        let first = owner.shutdown().await;
        let second = owner.shutdown().await;
        assert_eq!(first, second);
        assert!(matches!(
            first,
            Err(LifecycleShutdownError::TimedOut { unreleased_resources })
                if unreleased_resources == vec![
                    resource::WEB_SERVER.to_string(),
                    resource::MCP_SERVER.to_string(),
                ]
        ));
    }

    #[tokio::test]
    async fn concurrent_shutdown_joins_the_same_terminal_result() {
        let owner = LifecycleShutdownOwner::with_timeout(Duration::from_millis(50));
        owner
            .push_step(resource::MANAGED_CODEX_CHILD, async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err(LifecycleStepError::failed("child stuck"))
            })
            .await;

        let a = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.shutdown().await })
        };
        let b = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.shutdown().await })
        };
        let first = a.await.expect("join");
        let second = b.await.expect("join");
        assert_eq!(first, second);
        assert!(matches!(
            first,
            Err(LifecycleShutdownError::Failed {
                failed_resources,
                ..
            }) if failed_resources == vec![resource::MANAGED_CODEX_CHILD.to_string()]
        ));
    }

    #[tokio::test]
    async fn exhausted_deadline_still_attempts_later_resource_cleanup() {
        let later_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let owner = LifecycleShutdownOwner::with_timeout(Duration::from_millis(20));
        owner
            .push_step(resource::RUNTIME_TURN, async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(())
            })
            .await;
        {
            let later_ran = Arc::clone(&later_ran);
            owner
                .push_step(resource::CONTROL_LOCK, async move {
                    later_ran.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                })
                .await;
        }

        let result = owner.shutdown().await;
        assert!(
            later_ran.load(std::sync::atomic::Ordering::Acquire),
            "later resources must still run after an earlier step exhausts the deadline"
        );
        assert!(matches!(
            result,
            Err(LifecycleShutdownError::TimedOut { unreleased_resources })
                if unreleased_resources == vec![resource::RUNTIME_TURN.to_string()]
        ));
    }

    #[tokio::test]
    async fn cancelled_shutdown_publishes_terminal_result_for_waiters() {
        let owner = LifecycleShutdownOwner::with_timeout(Duration::from_secs(5));
        owner
            .push_step(resource::WEB_SERVER, async {
                std::future::pending::<Result<(), LifecycleStepError>>().await
            })
            .await;

        let initiator = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.shutdown().await })
        };
        // Ensure the initiator owns Running before we cancel it.
        tokio::task::yield_now().await;
        let waiter = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.shutdown().await })
        };
        initiator.abort();
        let _ = initiator.await;

        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter must not hang after initiator cancellation")
            .expect("waiter join");
        assert!(matches!(
            &result,
            Err(LifecycleShutdownError::Failed {
                failed_resources,
                detail
            }) if failed_resources.as_slice() == ["lifecycle_owner"]
                && detail.contains("cancelled")
        ));
        assert_eq!(owner.shutdown().await, result);
    }
}
