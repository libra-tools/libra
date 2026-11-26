//! Request-context regressions through the existing operation entry point.

use std::{fs, time::Duration};

use sea_orm::{ActiveModelTrait, ActiveValue::Set};
use serial_test::serial;

use super::{MutationClass, OperationError, OperationMetaV2, run_with_operation};
use crate::internal::{
    config::ConfigKv,
    db::create_database,
    model::reference,
    workspace::RepoIdentity,
    worktree_scope::{RequestScope, WorktreeScope},
};

#[path = "tests/scope_context_tests/lifecycle.rs"]
mod lifecycle;
#[path = "tests/scope_context_tests/overlap.rs"]
mod overlap;
#[path = "tests/scope_context_tests/success.rs"]
mod success;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

struct ScopeFixture {
    _directory: tempfile::TempDir,
    scope: RequestScope,
}

async fn fixture(persistent: bool) -> ScopeFixture {
    let directory = tempfile::tempdir().expect("request repository");
    let workdir = directory.path().canonicalize().expect("canonical root");
    let gitdir = workdir.join(".libra");
    fs::create_dir_all(gitdir.join("objects")).expect("object directory");
    fs::write(gitdir.join("HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
    if persistent {
        let connection = create_database(gitdir.join("libra.db").to_str().expect("database path"))
            .await
            .expect("repository schema");
        // Like init, each repository owns an identity in addition to its schema.
        let repo_id = uuid::Uuid::new_v4().to_string();
        ConfigKv::set_with_conn(&connection, "libra.repoid", &repo_id, false)
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
        assert_eq!(
            RepoIdentity::resolve(&connection)
                .await
                .expect("valid repository identity")
                .as_str(),
            repo_id
        );
        connection.close().await.expect("close fixture connection");
    } else {
        fs::write(gitdir.join("libra.db"), b"").expect("repository marker");
    }
    let scope = RequestScope::resolve(workdir).expect("resolved request scope");
    ScopeFixture {
        _directory: directory,
        scope,
    }
}

#[tokio::test]
#[serial]
async fn operation_scope_readonly_and_ephemeral_bind_the_supplied_request() {
    // Given both a real read-only repository and an explicitly resolved non-repository scope.
    let repository = fixture(false).await;
    let directory = tempfile::tempdir().expect("ephemeral root");
    let ephemeral = RequestScope {
        scope: WorktreeScope::Main,
        workdir: directory.path().to_path_buf(),
        gitdir: directory.path().join("absent-gitdir"),
        storage: directory.path().join("explicit-storage"),
        worktree_root: directory.path().to_path_buf(),
    };
    let baseline = WorktreeScope::request_scope();
    let mut observations = Vec::new();
    for (scope, class) in [
        (&repository.scope, MutationClass::ReadOnly),
        (&ephemeral, MutationClass::InternalWorker),
        (&ephemeral, MutationClass::WorkspaceMutation),
    ] {
        // When the public boundary executes a callback without a persistent transaction.
        let result = run_with_operation(scope, OperationMetaV2::default(), class, |_| async {
            Ok::<_, OperationError>(WorktreeScope::request_scope())
        })
        .await
        .expect("operation callback");
        observations.push((result.value, scope.clone()));
    }
    // Then explicit paths, including non-discoverable ones, survive without altering the caller.
    assert_eq!(WorktreeScope::request_scope(), baseline);
    for (observed, expected) in observations {
        assert_eq!(observed, Some(expected));
    }
}
