use std::sync::Arc;

use thiserror::Error;

use super::observer::{EpisodeObserver, MemoryDependencyObserver};
use crate::{
    internal::ai::{history::HistoryManager, keyed_digest::RepositoryKeyedDigest},
    utils::util::DATABASE,
};

/// Schedule a best-effort observer repair after a terminal event commit.
/// The terminal write has already succeeded and is never held open by Memory
/// scanning, digest loading, provider work, or retry handling.
pub(crate) fn schedule_observer_repair(history: Option<Arc<HistoryManager>>) {
    let Some(history) = history else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("Memory observer wake skipped because no Tokio runtime is active");
        return;
    };
    handle.spawn(async move {
        if repair_observers(history.as_ref()).await.is_err() {
            tracing::warn!("Memory observer wake failed; startup/status repair will retry it");
        }
    });
}

/// Reconcile both durable observer cursors with their current authoritative
/// refs. Runtime startup and status paths can call this same idempotent seam;
/// a crash before the short cursor transaction simply repeats the scan.
pub(crate) async fn repair_observers(history: &HistoryManager) -> Result<(), ObserverRepairError> {
    let digest =
        RepositoryKeyedDigest::load_or_initialize(&history.repository_path().join(DATABASE))
            .await
            .map_err(|_| ObserverRepairError::new(ObserverRepairErrorKind::DigestUnavailable))?;
    repair_observers_with_digest(history, digest.as_ref()).await
}

/// Runtime seam for callers that already own the repository-pinned digest.
/// Reusing it avoids a second database open/cache lookup at every lifecycle
/// wake and keeps observer/job receipts on one identity.
pub(super) async fn repair_observers_with_digest(
    history: &HistoryManager,
    digest: &RepositoryKeyedDigest,
) -> Result<(), ObserverRepairError> {
    let database = history.database_connection();
    EpisodeObserver::new(history, &database, digest, "repo")
        .map_err(|_| ObserverRepairError::configuration())?
        .observe_terminal_events()
        .await
        .map_err(|_| ObserverRepairError::observation())?;
    MemoryDependencyObserver::new(history, &database, digest, "repo")
        .map_err(|_| ObserverRepairError::configuration())?
        .observe_task_revisions()
        .await
        .map_err(|_| ObserverRepairError::observation())?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObserverRepairErrorKind {
    InvalidConfiguration,
    DigestUnavailable,
    ObservationFailed,
}

#[derive(Debug, Error)]
#[error("Memory observer repair failed ({kind:?})")]
pub(crate) struct ObserverRepairError {
    kind: ObserverRepairErrorKind,
}

impl ObserverRepairError {
    const fn new(kind: ObserverRepairErrorKind) -> Self {
        Self { kind }
    }

    const fn configuration() -> Self {
        Self::new(ObserverRepairErrorKind::InvalidConfiguration)
    }

    const fn observation() -> Self {
        Self::new(ObserverRepairErrorKind::ObservationFailed)
    }

    pub(crate) const fn kind(&self) -> ObserverRepairErrorKind {
        self.kind
    }
}
