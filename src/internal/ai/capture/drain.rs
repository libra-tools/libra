//! Single consumer for `.libra/private/capture-spool/ready/`.
//!
//! The process that cannot take the consumer lock returns without reading a
//! ready file. A ready file is deleted only after the catalog commit succeeds.

#[cfg(test)]
use std::sync::Mutex;
use std::{fs, path::Path};

use anyhow::{Result, anyhow};

use super::{live_pipeline::SpoolReplayResolver, spool, worker};

/// Lock name. It is not the recovery worker lock and not `capacity.lock`.
pub(crate) const CONSUMER_LOCK: &str = "capture-spool-consumer.lock";

/// The capture database did not accept a writer before the busy timeout.
#[derive(Debug)]
pub(crate) struct CaptureDatabaseBusy;

impl std::fmt::Display for CaptureDatabaseBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "the capture database is busy; queued events stay durable and will be retried",
        )
    }
}

impl std::error::Error for CaptureDatabaseBusy {}

/// `capture.db` was written by a newer Libra than this binary can migrate.
#[derive(Debug)]
pub(crate) struct CaptureSchemaTooNew;

impl std::fmt::Display for CaptureSchemaTooNew {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "the capture database schema is too new for this binary; install the current Libra release and run libra __capture-spool-drain",
        )
    }
}

impl std::error::Error for CaptureSchemaTooNew {}

/// The consumer moved queued events that could not be stored to
/// `quarantine/`. Every other ready file was still processed.
#[derive(Debug)]
pub(crate) struct CaptureEventsQuarantined {
    pub(crate) count: usize,
}

impl std::fmt::Display for CaptureEventsQuarantined {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} queued capture event(s) could not be stored and were moved to quarantine; run `libra agent doctor`",
            self.count
        )
    }
}

impl std::error::Error for CaptureEventsQuarantined {}

/// Test-only record of `(storage, ready file)` reads, in read order.
#[cfg(test)]
static SPOOL_READS: Mutex<Vec<(std::path::PathBuf, String)>> = Mutex::new(Vec::new());

/// Drain `ready/` for the repository that contains the current directory.
///
/// Success and a busy consumer both print nothing. Failures use a fixed
/// message and never include an envelope body.
pub(crate) async fn run_from_repository(resolve: SpoolReplayResolver) -> Result<()> {
    let storage = crate::utils::util::try_get_storage_path(None)?;
    drain_storage(&storage, resolve).await
}

/// Replay every ready file in filename order, or return immediately when
/// another consumer holds the lock.
///
/// A hook that published while this consumer held the lock may have had its
/// own consumer lose the lock race. After releasing the lock the consumer
/// therefore looks at `ready/` again and takes the lock once more when a file
/// is still waiting, so no published envelope is left without a consumer.
pub(crate) async fn drain_storage(storage: &Path, resolve: SpoolReplayResolver) -> Result<()> {
    let mut quarantined = 0_usize;
    loop {
        let Some(lock) = worker::try_lock_named(storage, CONSUMER_LOCK)
            .map_err(|_| anyhow!("capture queue consumer lock could not be taken"))?
        else {
            break;
        };
        quarantined = quarantined.saturating_add(drain_locked(storage, resolve).await?);
        drop(lock);
        if spool::ready_names(storage)?.is_empty() {
            break;
        }
    }
    if quarantined == 0 {
        Ok(())
    } else {
        Err(CaptureEventsQuarantined { count: quarantined }.into())
    }
}

/// Apply `ready/` until it is empty. Each file either commits and is deleted,
/// moves to `quarantine/`, or stops the drain with a retryable error and stays
/// in `ready/`. Returns how many files were quarantined.
async fn drain_locked(storage: &Path, resolve: SpoolReplayResolver) -> Result<usize> {
    let mut quarantined = 0_usize;
    loop {
        let names = spool::ready_names(storage)?;
        if names.is_empty() {
            return Ok(quarantined);
        }
        for name in names {
            note_ready_read(storage, &name);
            let path = spool::ready_dir(storage).join(&name);
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(anyhow!("capture queue entry could not be read")),
            };
            match super::live_pipeline::persist_spool_envelope(&bytes, storage, resolve).await {
                Ok(()) => {
                    fs::remove_file(&path)
                        .map_err(|_| anyhow!("capture queue could not retire a stored event"))?;
                }
                Err(error) if is_retryable(&error) => return Err(error),
                Err(_) => {
                    fs::rename(&path, spool::quarantine_dir(storage).join(&name)).map_err(
                        |_| anyhow!("capture queue could not quarantine an unusable event"),
                    )?;
                    quarantined = quarantined.saturating_add(1);
                    tracing::warn!(
                        target: "agent.capture.spool",
                        reason = "quarantined",
                        "a queued capture event could not be stored"
                    );
                }
            }
        }
    }
}

fn note_ready_read(storage: &Path, name: &str) {
    #[cfg(test)]
    if let Ok(mut reads) = SPOOL_READS.lock() {
        reads.push((storage.to_path_buf(), name.to_string()));
    }
    #[cfg(not(test))]
    let _ = (storage, name);
}

/// Failures that are not about the envelope itself. The file stays in
/// `ready/` and a later consumer retries it; only envelope-specific failures
/// move a file to `quarantine/`.
fn is_retryable(error: &anyhow::Error) -> bool {
    if super::scope_binding::active_repository_failure_class(error).is_some() {
        return true;
    }
    error.chain().any(|cause| {
        // Database classes and filesystem failures (full disk, an unwritable
        // object store) describe the environment, not the envelope.
        if cause.is::<CaptureDatabaseBusy>()
            || cause.is::<CaptureSchemaTooNew>()
            || cause.is::<std::io::Error>()
        {
            return true;
        }
        let text = cause.to_string().to_ascii_lowercase();
        [
            "database is locked",
            "database is busy",
            "readonly database",
            "disk i/o error",
            "database or disk is full",
            "unable to open database",
        ]
        .iter()
        .any(|pattern| text.contains(pattern))
    })
}

/// Map an open failure of `capture.db` to the stable capture classes.
pub(crate) fn classify_capture_open(error: &std::io::Error) -> anyhow::Error {
    let text = error.to_string();
    if text.contains("schema version") && text.contains("newer than this Libra") {
        anyhow!(CaptureSchemaTooNew)
    } else if error.kind() == std::io::ErrorKind::TimedOut
        || text.to_ascii_lowercase().contains("locked")
        || text.to_ascii_lowercase().contains("busy")
    {
        anyhow!(CaptureDatabaseBusy)
    } else if text.contains("schema version") {
        anyhow!(CaptureSchemaTooNew)
    } else {
        anyhow!("the capture database could not be opened")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ai::capture::spool::capture_tests_enabled;

    /// The consumer lock requires a private `private/` directory.
    fn private_dirs(storage: &Path) {
        let private = storage.join("private");
        fs::create_dir_all(&private).expect("private");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).expect("private mode");
        }
    }

    fn reads_for(storage: &Path) -> Vec<String> {
        SPOOL_READS
            .lock()
            .expect("reads")
            .iter()
            .filter(|(root, _)| root == storage)
            .map(|(_, name)| name.clone())
            .collect()
    }

    #[test]
    fn capture_spool_busy_and_schema_text_differ_from_queue_full() {
        if !capture_tests_enabled("capture_spool_busy_and_schema_text_differ_from_queue_full") {
            return;
        }
        let busy = CaptureDatabaseBusy.to_string();
        let schema = CaptureSchemaTooNew.to_string();
        assert!(!busy.contains("newer Libra"));
        assert!(!schema.contains("newer Libra"));
        assert!(!busy.contains("queue is full"));
        assert!(schema.contains("too new"));
        assert!(busy.contains("busy"));
        assert_ne!(busy, schema);
    }

    #[tokio::test]
    async fn capture_spool_second_consumer_does_not_read_ready() {
        if !capture_tests_enabled("capture_spool_second_consumer_does_not_read_ready") {
            return;
        }
        let root = tempfile::tempdir().expect("consumer fixture");
        let ready = spool::ready_dir(root.path());
        private_dirs(root.path());
        fs::create_dir_all(&ready).expect("ready");
        fs::write(ready.join("0-first~one"), b"CANARY-BODY").expect("seed");
        fs::write(ready.join("1-second~two"), b"other").expect("seed");
        let _held = worker::try_lock_named(root.path(), CONSUMER_LOCK)
            .expect("lock")
            .expect("available");
        drain_storage(root.path(), crate::command::hooks::spool_replay_provider)
            .await
            .expect("loser returns");
        assert!(reads_for(root.path()).is_empty());
        let body = fs::read(ready.join("0-first~one")).expect("untouched");
        assert_eq!(body, b"CANARY-BODY");
    }

    #[tokio::test]
    async fn capture_spool_filename_order_consumes_terminal_prefix_first() {
        if !capture_tests_enabled("capture_spool_filename_order_consumes_terminal_prefix_first") {
            return;
        }
        let root = tempfile::tempdir().expect("order fixture");
        let ready = spool::ready_dir(root.path());
        private_dirs(root.path());
        fs::create_dir_all(spool::quarantine_dir(root.path())).expect("quarantine");
        fs::create_dir_all(&ready).expect("ready");
        fs::write(ready.join("1-0000000000001-a~ordinary"), b"not-json").expect("ordinary");
        fs::write(ready.join("0-0000000000002-b~terminal"), b"not-json").expect("terminal");
        let error = drain_storage(root.path(), crate::command::hooks::spool_replay_provider)
            .await
            .expect_err("undecodable envelopes are reported as quarantined");
        assert_eq!(
            error
                .downcast_ref::<CaptureEventsQuarantined>()
                .map(|quarantined| quarantined.count),
            Some(2)
        );
        let order = reads_for(root.path());
        assert_eq!(
            order,
            vec![
                "0-0000000000002-b~terminal".to_string(),
                "1-0000000000001-a~ordinary".to_string()
            ]
        );
        // Undecodable envelopes are envelope-specific failures: they move to
        // quarantine instead of blocking the queue.
        assert!(spool::ready_names(root.path()).expect("ready").is_empty());
        let mut quarantined = fs::read_dir(spool::quarantine_dir(root.path()))
            .expect("quarantine")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        quarantined.sort();
        assert_eq!(quarantined.len(), 2, "{quarantined:?}");
    }
}
