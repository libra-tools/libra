//! Repository source and pending epoch binding for Mega2 traces (LTR-10).
//!
//! The source is the repository `RepoIdentity` (`libra.repoid`), taken
//! verbatim. The first epoch is written to the repository config database
//! before this function returns, and this module never opens a socket.
//! A later epoch is stored only for an explicit new-stream request.
//! Linked worktrees share the common repository database, so they share
//! the same source and epoch. An independent clone has its own database
//! and therefore its own source.

use std::{fmt, path::Path};

use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionSession, TransactionTrait};
use uuid::Uuid;

use crate::{
    internal::{config::ConfigKv, db, workspace::RepoIdentity},
    utils::util::{self, DATABASE},
};

pub const SOURCE_KEY: &str = "libra.traces.mega2.source";
pub const EPOCH_KEY: &str = "libra.traces.mega2.epoch";

/// How the caller wants the persisted epoch to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochIntent {
    /// Keep the persisted epoch. Create the first one when none exists.
    Current,
    /// Store a new epoch. This is the only way to replace an existing one.
    NewStream,
}

/// Source and epoch that are already durable in the repository database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEpochBinding {
    pub source: String,
    pub epoch: String,
}

#[derive(Debug)]
pub enum Mega2StreamError {
    Source(String),
    Store(String),
    Epoch(String),
}

impl fmt::Display for Mega2StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(message) | Self::Store(message) | Self::Epoch(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for Mega2StreamError {}

/// Read `libra.repoid` for `workdir`, following a linked worktree's common
/// storage. Does not create an identity or an epoch.
pub async fn read_repository_source_at(workdir: &Path) -> Result<String, Mega2StreamError> {
    let connection = open_repository(workdir).await?;
    let identity = RepoIdentity::resolve(&connection)
        .await
        .map_err(|error| Mega2StreamError::Source(error.to_string()))?;
    Ok(identity.as_str().to_string())
}

/// Persist source and epoch for the repository that owns `workdir`.
///
/// The write commits before this function returns. There is no network I/O.
pub async fn bind_source_epoch_at(
    workdir: &Path,
    intent: EpochIntent,
) -> Result<SourceEpochBinding, Mega2StreamError> {
    let connection = open_repository(workdir).await?;
    bind_source_epoch(&connection, intent).await
}

/// Persist source and epoch on an already-open repository connection.
///
/// Call this before any Mega2 network attempt. A failed identity check
/// writes nothing.
pub async fn bind_source_epoch<C>(
    connection: &C,
    intent: EpochIntent,
) -> Result<SourceEpochBinding, Mega2StreamError>
where
    C: ConnectionTrait + TransactionTrait,
{
    let identity = RepoIdentity::resolve(connection)
        .await
        .map_err(|error| Mega2StreamError::Source(error.to_string()))?;
    let source = identity.as_str().to_string();
    if source.trim().is_empty() || source != source.trim() {
        return Err(Mega2StreamError::Source(
            "repository identity (libra.repoid) is missing or has surrounding whitespace; \
             restore it with `libra init` in a fresh clone or rewrite the stored value \
             without padding"
                .to_string(),
        ));
    }

    let transaction = connection.begin().await.map_err(|error| {
        Mega2StreamError::Store(format!(
            "cannot start a repository transaction for the traces source binding: {error}"
        ))
    })?;

    let stored_source = config_value(&transaction, SOURCE_KEY).await?;
    if let Some(stored) = stored_source.as_deref() {
        if stored != source {
            return Err(Mega2StreamError::Source(format!(
                "repository source changed from the recorded traces binding; refusing to \
                 rewrite it silently (recorded {stored:?}, repository {source:?}). Restore \
                 the original `libra.repoid` or use a separate clone"
            )));
        }
    } else {
        ConfigKv::set_with_conn(&transaction, SOURCE_KEY, &source, false)
            .await
            .map_err(|error| {
                Mega2StreamError::Store(format!(
                    "cannot record the traces source in repository config: {error}"
                ))
            })?;
    }

    let stored_epoch = config_value(&transaction, EPOCH_KEY).await?;
    let epoch = match (stored_epoch, intent) {
        (Some(existing), EpochIntent::Current) => {
            validate_epoch(&existing)?;
            existing
        }
        (None, EpochIntent::Current) | (_, EpochIntent::NewStream) => {
            let created = Uuid::new_v4().to_string();
            ConfigKv::set_with_conn(&transaction, EPOCH_KEY, &created, false)
                .await
                .map_err(|error| {
                    Mega2StreamError::Store(format!(
                        "cannot record the traces epoch in repository config: {error}"
                    ))
                })?;
            created
        }
    };

    transaction.commit().await.map_err(|error| {
        Mega2StreamError::Store(format!(
            "cannot commit the traces source and epoch binding: {error}"
        ))
    })?;

    Ok(SourceEpochBinding { source, epoch })
}

fn validate_epoch(value: &str) -> Result<(), Mega2StreamError> {
    Uuid::parse_str(value).map(|_| ()).map_err(|_| {
        Mega2StreamError::Epoch(format!(
            "stored traces epoch {value:?} is not a UUID; inspect `{EPOCH_KEY}` and replace \
             it with `libra agent push --mega2 --new-stream` once that command is available"
        ))
    })
}

async fn config_value<C: ConnectionTrait>(
    connection: &C,
    key: &str,
) -> Result<Option<String>, Mega2StreamError> {
    let entry = ConfigKv::get_with_conn(connection, key)
        .await
        .map_err(|error| {
            Mega2StreamError::Store(format!("cannot read repository config `{key}`: {error}"))
        })?;
    Ok(entry.map(|entry| entry.value))
}

async fn open_repository(workdir: &Path) -> Result<DatabaseConnection, Mega2StreamError> {
    let storage = util::try_get_storage_path(Some(workdir.to_path_buf())).map_err(|error| {
        Mega2StreamError::Store(format!(
            "cannot find the repository storage for {}: {error}. Run `libra init` in that \
             directory, or use the main worktree if this linked worktree is detached",
            workdir.display()
        ))
    })?;
    let database = storage.join(DATABASE);
    db::get_db_conn_instance_for_path(&database)
        .await
        .map_err(|error| {
            Mega2StreamError::Store(format!(
                "cannot open the repository database {}: {error}",
                database.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::internal::{config::ConfigKv, db::create_database};

    async fn repository_with_identity(root: &Path, identity: Option<&str>) {
        let gitdir = root.join(".libra");
        fs::create_dir_all(gitdir.join("objects")).expect("objects");
        fs::write(gitdir.join("HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
        let database = gitdir.join(DATABASE);
        let connection = create_database(database.to_str().expect("utf8 database path"))
            .await
            .expect("schema");
        if let Some(identity) = identity {
            ConfigKv::set_with_conn(&connection, "libra.repoid", identity, false)
                .await
                .expect("identity");
        }
        connection.close().await.expect("close");
    }

    fn file_stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
        let meta = fs::metadata(path).ok()?;
        Some((meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH)))
    }

    #[tokio::test]
    async fn source_epoch_binding_matrix() {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let global_before = home
            .as_ref()
            .map(|home| file_stamp(&home.join(".libra/config.db")));
        let system_before = file_stamp(Path::new("/etc/libra/config.db"));

        let root = tempfile::tempdir().expect("repository");
        repository_with_identity(root.path(), None).await;
        let missing = bind_source_epoch_at(root.path(), EpochIntent::Current)
            .await
            .expect_err("missing repoid");
        assert!(
            missing.to_string().contains("libra.repoid") && missing.to_string().contains("init"),
            "missing identity must be actionable: {missing}"
        );

        let padded = tempfile::tempdir().expect("padded repository");
        repository_with_identity(padded.path(), Some("  padded-repoid  ")).await;
        let padded_error = bind_source_epoch_at(padded.path(), EpochIntent::Current)
            .await
            .expect_err("padded repoid");
        let padded_text = padded_error.to_string();
        assert!(
            padded_text.contains("whitespace") || padded_text.contains("padding"),
            "padded identity must be rejected: {padded_text}"
        );
        let padded_db = open_repository(padded.path()).await.expect("reopen");
        assert!(
            config_value(&padded_db, EPOCH_KEY)
                .await
                .expect("read epoch")
                .is_none(),
            "a rejected identity must not persist an epoch"
        );

        let legacy = tempfile::tempdir().expect("legacy repository");
        let legacy_id = "opaque-legacy-value";
        repository_with_identity(legacy.path(), Some(legacy_id)).await;
        let first = bind_source_epoch_at(legacy.path(), EpochIntent::Current)
            .await
            .expect("first epoch");
        assert_eq!(first.source, legacy_id);
        Uuid::parse_str(&first.epoch).expect("epoch uuid");
        let reread = open_repository(legacy.path()).await.expect("reopen");
        assert_eq!(
            config_value(&reread, EPOCH_KEY)
                .await
                .expect("epoch")
                .as_deref(),
            Some(first.epoch.as_str()),
            "the first epoch is durable before any network attempt"
        );
        let reused = bind_source_epoch_at(legacy.path(), EpochIntent::Current)
            .await
            .expect("reuse");
        assert_eq!(reused.epoch, first.epoch);
        let rotated = bind_source_epoch_at(legacy.path(), EpochIntent::NewStream)
            .await
            .expect("explicit new stream");
        assert_ne!(rotated.epoch, first.epoch);
        assert_eq!(rotated.source, legacy_id);
        let kept = bind_source_epoch_at(legacy.path(), EpochIntent::Current)
            .await
            .expect("keep rotated epoch");
        assert_eq!(kept.epoch, rotated.epoch);

        let connection = open_repository(legacy.path()).await.expect("rewrite check");
        ConfigKv::set_with_conn(&connection, "libra.repoid", "other-source", false)
            .await
            .expect("simulate a silent rewrite");
        let refused = bind_source_epoch(&connection, EpochIntent::Current)
            .await
            .expect_err("silent source change");
        assert!(
            refused.to_string().contains("silently"),
            "a changed source must stay unchanged: {refused}"
        );
        assert_eq!(
            config_value(&connection, SOURCE_KEY)
                .await
                .expect("source")
                .as_deref(),
            Some(legacy_id)
        );

        let linked_root = tempfile::tempdir().expect("linked parent");
        repository_with_identity(linked_root.path(), Some("shared-source")).await;
        let linked = tempfile::tempdir().expect("linked worktree");
        let linked_gitdir = linked.path().join(".libra");
        fs::create_dir_all(&linked_gitdir).expect("linked gitdir");
        fs::write(
            linked_gitdir.join("commondir"),
            format!("{}\n", linked_root.path().join(".libra").display()),
        )
        .expect("commondir");
        fs::write(linked_gitdir.join("worktree_id"), b"linked-test\n").expect("worktree id");
        let from_main = read_repository_source_at(linked_root.path())
            .await
            .expect("main source");
        let from_link = read_repository_source_at(linked.path())
            .await
            .expect("linked source");
        assert_eq!(from_main, from_link);
        let linked_binding = bind_source_epoch_at(linked.path(), EpochIntent::Current)
            .await
            .expect("bind through the linked worktree");
        let main_binding = bind_source_epoch_at(linked_root.path(), EpochIntent::Current)
            .await
            .expect("read the same epoch from the main worktree");
        assert_eq!(linked_binding, main_binding);
        assert_eq!(linked_binding.source, "shared-source");

        let other = tempfile::tempdir().expect("independent clone");
        repository_with_identity(other.path(), Some("clone-source")).await;
        let clone_binding = bind_source_epoch_at(other.path(), EpochIntent::Current)
            .await
            .expect("independent clone");
        assert_ne!(clone_binding.source, linked_binding.source);
        assert_ne!(clone_binding.epoch, linked_binding.epoch);

        let global_after = home
            .as_ref()
            .map(|home| file_stamp(&home.join(".libra/config.db")));
        assert_eq!(
            global_before, global_after,
            "global config must stay unchanged"
        );
        assert_eq!(
            system_before,
            file_stamp(Path::new("/etc/libra/config.db")),
            "system config must stay unchanged"
        );
    }
}
