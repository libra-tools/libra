//! Request-local progress tracking for inactivity deadlines.
use std::{future::Future, time::Duration};

use tokio::sync::watch;

pub(crate) const EPISODE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

tokio::task_local! {
    static PROGRESS: watch::Sender<()>;
}

pub(crate) fn record() {
    let _ = PROGRESS.try_with(|sender| sender.send_replace(()));
}

/// Bound inactivity, not total runtime, including for non-Send bridge handlers.
pub(crate) async fn with_idle_timeout<F: Future>(
    idle_timeout: Duration,
    future: F,
) -> Result<F::Output, ()> {
    let (sender, mut receiver) = watch::channel(());
    PROGRESS
        .scope(sender, async move {
            tokio::pin!(future);
            loop {
                tokio::select! {
                    result = &mut future => return Ok(result),
                    changed = tokio::time::timeout(idle_timeout, receiver.changed()) => {
                        if !matches!(changed, Ok(Ok(()))) {
                            return Err(());
                        }
                    }
                }
            }
        })
        .await
}
