//! Local Git protocol client tests covering CWD restoration on error paths.
//!
//! Pins the invariant that `LocalClient::discovery_reference` always restores the
//! caller's working directory, even when the underlying ref storage is corrupt.
//! This matters because the local client temporarily `chdir`s into the target repo
//! to discover refs; a leaked CWD change would break every subsequent `libra`
//! command in the same process.
//!
//! **Layer:** L1 — deterministic, no external dependencies. `#[serial]` because the
//! test mutates CWD via `ChangeDirGuard` and depends on the recovery contract.

use std::env;

use libra::{
    command::commit::{self, CommitArgs},
    git_protocol::ServiceType,
    internal::{branch::Branch, protocol::local_client::LocalClient},
    utils::test::{self, ChangeDirGuard},
};
use serial_test::serial;
use tempfile::tempdir;

/// Scenario: deliberately corrupt a branch (`Branch::update_branch("broken",
/// "not-a-valid-hash", None)`) inside a target repo, then call
/// `LocalClient::discovery_reference` from a different `caller_dir` and confirm:
/// - The discovery returns an error containing "corrupt" (so callers can pattern
///   match on the failure mode).
/// - The process CWD is unchanged afterwards — the local client must restore the
///   directory it `chdir`'d away from before propagating the error.
///
/// Acts as a regression guard against leaking CWD changes on protocol errors.
/// `#[serial]` because two `ChangeDirGuard` instances live in this test.
#[tokio::test]
#[serial(cwd)]
async fn discovery_reference_restores_current_dir_after_error() {
    let caller_dir = tempdir().unwrap();
    let repo_dir = tempdir().unwrap();

    test::setup_with_new_libra_in(repo_dir.path()).await;
    {
        let _guard = ChangeDirGuard::new(repo_dir.path());
        commit::execute(CommitArgs {
            message: Some("initial".into()),
            allow_empty: true,
            disable_pre: true,
            ..Default::default()
        })
        .await;

        Branch::update_branch("broken", "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    let _caller_guard = ChangeDirGuard::new(caller_dir.path());
    let original_dir = env::current_dir().unwrap();

    let client = LocalClient::from_path(repo_dir.path()).unwrap();
    let error = client
        .discovery_reference(ServiceType::UploadPack)
        .await
        .expect_err("expected corrupt branch storage to fail discovery");

    assert!(
        error.to_string().contains("corrupt"),
        "unexpected error: {error}"
    );
    assert_eq!(
        env::current_dir().unwrap(),
        original_dir,
        "local protocol discovery should restore the original working directory",
    );
}
