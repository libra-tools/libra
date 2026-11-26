//! Isolated repository fixtures and semantic lease assertions.

use std::{fs, path::PathBuf};

use git_internal::internal::index::Index;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DbBackend, Statement};

use super::{OperationError, PinnedRequestScope, ScopeLease};
use crate::internal::{
    config::ConfigKv,
    db::{create_database, get_db_conn_instance_for_path},
    model::reference,
    worktree_scope::WorktreeScope,
};

pub(super) const REPO_ID: &str = "c8000000-0000-4000-8000-000000000001";

pub(super) struct Fixture {
    pub root: PathBuf,
    pub main: PinnedRequestScope,
}

impl Fixture {
    pub async fn open(persistent: bool) -> Self {
        let root = std::env::current_dir().expect("isolated child worktree");
        let storage = root.join(".libra");
        fs::create_dir_all(storage.join("objects")).expect("object directory");
        fs::write(storage.join("HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
        Index::new()
            .save(storage.join("index"))
            .expect("valid empty index");
        if persistent {
            let connection =
                create_database(storage.join("libra.db").to_str().expect("database path"))
                    .await
                    .expect("valid repository schema");
            ConfigKv::set_with_conn(&connection, "libra.repoid", REPO_ID, false)
                .await
                .expect("repository identity");
            reference::ActiveModel {
                name: Set(Some("main".to_string())),
                kind: Set(reference::ConfigKind::Head),
                commit: Set(None),
                remote: Set(None),
                worktree_id: Set(None),
                ..Default::default()
            }
            .insert(&connection)
            .await
            .expect("main unborn HEAD");
            connection.close().await.expect("close setup connection");
        }
        Self {
            main: PinnedRequestScope {
                scope: WorktreeScope::Main,
                workdir: root.clone(),
                worktree_root: root.clone(),
                gitdir: storage.clone(),
                storage,
            },
            root,
        }
    }

    pub async fn linked_scope(&self) -> PinnedRequestScope {
        let workdir = self.root.parent().expect("sandbox root").join("linked");
        fs::create_dir_all(workdir.join(".libra")).expect("private linked gitdir");
        let workdir = workdir.canonicalize().expect("canonical linked worktree");
        let gitdir = workdir.join(".libra");
        let worktree_id = crate::utils::util::worktree_instance_id(&workdir);
        fs::write(gitdir.join("worktree_id"), &worktree_id).expect("linked identity");
        fs::write(
            gitdir.join("commondir"),
            self.main.storage.to_string_lossy().as_bytes(),
        )
        .expect("linked common storage");
        fs::write(gitdir.join("HEAD"), b"ref: refs/heads/linked\n").expect("linked HEAD");
        Index::new()
            .save(gitdir.join("index"))
            .expect("linked index");
        let scope = PinnedRequestScope::try_resolve(workdir.clone())
            .expect("resolve linked request scope")
            .expect("linked repository was discovered");
        assert_eq!(scope.scope, WorktreeScope::Linked(worktree_id));
        assert_eq!(scope.workdir, workdir);
        assert_eq!(scope.worktree_root, workdir);
        assert_eq!(scope.gitdir, gitdir);
        assert_eq!(scope.storage, self.main.storage);
        assert_ne!(scope.gitdir, self.main.gitdir);
        let connection = get_db_conn_instance_for_path(&scope.storage.join("libra.db"))
            .await
            .expect("linked repository connection");
        reference::ActiveModel {
            name: Set(Some("linked".to_string())),
            kind: Set(reference::ConfigKind::Head),
            commit: Set(None),
            remote: Set(None),
            worktree_id: Set(scope.scope.worktree_id().map(str::to_string)),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("linked unborn HEAD");
        scope
    }

    pub fn ready(&self) {
        fs::write(
            std::env::var_os(super::READY_ENV).expect("ready path"),
            b"ready",
        )
        .expect("behavior readiness marker");
    }
}

pub(super) fn lock_path(scope: &PinnedRequestScope) -> PathBuf {
    scope.gitdir.join("info/operation-v2.lock")
}

pub(super) fn busy_error(error: &OperationError, scope: &PinnedRequestScope) {
    assert!(matches!(error, OperationError::LeaseBusy { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("already held"), "{message}");
    assert!(
        message.contains(&format!("{REPO_ID}:{}", scope.scope.storage_key())),
        "{message}"
    );
    assert!(
        message.contains(&lock_path(scope).display().to_string()),
        "{message}"
    );
    assert!(message.contains("retry"), "{message}");
}

pub(super) fn refused(result: Result<ScopeLease, OperationError>) -> OperationError {
    match result {
        Ok(lease) => {
            drop(lease);
            panic!("scope lease unexpectedly succeeded");
        }
        Err(error) => error,
    }
}

pub(super) async fn journal_count(scope: &PinnedRequestScope) -> i64 {
    let connection = get_db_conn_instance_for_path(&scope.storage.join("libra.db"))
        .await
        .expect("repository connection");
    connection
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM operation_journal",
        ))
        .await
        .expect("journal query")
        .expect("count row")
        .try_get("", "count")
        .expect("journal count")
}
