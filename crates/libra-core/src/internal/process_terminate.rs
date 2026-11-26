//! Shared SIGINT/SIGTERM process-terminate gate.
//!
//! Extracted from the retired TUI module (W5-03): the gate never depended on
//! any UI — it installs signal listeners and lets long-running foreground
//! commands (`libra code`'s Web lifecycle) await the first terminate signal
//! and coordinate a graceful shutdown.

use std::{sync::Arc, time::Instant};

/// Shared SIGTERM gate for foreground Code lifecycle shutdown.
#[derive(Clone, Debug)]
pub struct ProcessTerminateGate {
    signaled: Arc<std::sync::atomic::AtomicBool>,
    signaled_at: Arc<std::sync::Mutex<Option<Instant>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl ProcessTerminateGate {
    /// Install SIGINT and (on Unix) SIGTERM listeners synchronously, then wait
    /// for the first signal on a background task.
    pub fn install() -> anyhow::Result<Self> {
        let gate = Self {
            signaled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            signaled_at: Arc::new(std::sync::Mutex::new(None)),
            notify: Arc::new(tokio::sync::Notify::new()),
        };
        let signaled = Arc::clone(&gate.signaled);
        let signaled_at = Arc::clone(&gate.signaled_at);
        let notify = Arc::clone(&gate.notify);

        #[cfg(unix)]
        {
            let mut sigterm = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::terminate(),
            )
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to subscribe to SIGTERM for Code process terminate gate: {error}"
                )
            })?;
            let mut sigint = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::interrupt(),
            )
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to subscribe to SIGINT for Code process terminate gate: {error}"
                )
            })?;
            tokio::spawn(async move {
                tokio::select! {
                    _ = sigterm.recv() => {}
                    _ = sigint.recv() => {}
                }
                {
                    let mut stamped = signaled_at
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if stamped.is_none() {
                        *stamped = Some(Instant::now());
                    }
                }
                signaled.store(true, std::sync::atomic::Ordering::Release);
                notify.notify_waiters();
            });
        }

        #[cfg(not(unix))]
        {
            tokio::spawn(async move {
                if let Err(error) = tokio::signal::ctrl_c().await {
                    tracing::warn!(
                        error = %error,
                        "failed while waiting for SIGINT/ctrl_c in Code process terminate gate"
                    );
                }
                {
                    let mut stamped = signaled_at
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if stamped.is_none() {
                        *stamped = Some(Instant::now());
                    }
                }
                signaled.store(true, std::sync::atomic::Ordering::Release);
                notify.notify_waiters();
            });
        }

        Ok(gate)
    }

    pub fn is_signaled(&self) -> bool {
        self.signaled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Instant the first terminate signal was observed, if any.
    pub fn signaled_at(&self) -> Option<Instant> {
        *self
            .signaled_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub async fn wait(&self) {
        loop {
            // Creating `Notified` does not register a waiter until it is
            // enabled/polled. Enable first, then read the flag, otherwise a
            // SIGINT/SIGTERM between the load and registration is lost and
            // web-only `wait()` can hang forever.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_signaled() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod process_terminate_gate_tests {
    use std::{sync::Arc, time::Duration};

    use super::ProcessTerminateGate;

    fn test_gate() -> ProcessTerminateGate {
        ProcessTerminateGate {
            signaled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            signaled_at: Arc::new(std::sync::Mutex::new(None)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    #[tokio::test]
    async fn wait_returns_when_already_signaled_without_notify() {
        let gate = test_gate();
        gate.signaled
            .store(true, std::sync::atomic::Ordering::Release);
        tokio::time::timeout(Duration::from_millis(100), gate.wait())
            .await
            .expect("already-signaled wait must return without a notification");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_does_not_miss_concurrent_terminate_signal() {
        for _ in 0..100 {
            let gate = test_gate();
            let waiter = {
                let gate = gate.clone();
                tokio::spawn(async move {
                    gate.wait().await;
                })
            };
            let signaler = {
                let gate = gate.clone();
                tokio::spawn(async move {
                    gate.signaled
                        .store(true, std::sync::atomic::Ordering::Release);
                    {
                        let mut stamped = gate
                            .signaled_at
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        *stamped = Some(std::time::Instant::now());
                    }
                    gate.notify.notify_waiters();
                })
            };
            signaler.await.expect("signaler join");
            tokio::time::timeout(Duration::from_millis(500), waiter)
                .await
                .expect("wait must observe concurrent terminate without lost wakeup")
                .expect("waiter join");
        }
    }
}
