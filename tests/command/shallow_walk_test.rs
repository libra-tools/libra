//! M-WALK2: remaining history commands treat `.libra/shallow` as walk roots.

use std::fs;

use serial_test::serial;

use super::{
    ChangeDirGuard, assert_cli_success, loose_object_path, run_libra_command, status_test,
};

/// Three-commit repo: `origin/main` at the middle commit, HEAD one ahead.
/// When `shallow` is true the middle commit is the boundary and the oldest
/// object is removed so a walk past the boundary would fail.
async fn diverged_shallow_fixture(shallow: bool) -> (tempfile::TempDir, String, String) {
    let repo = status_test::upstream_tracking_repo(3);
    let _cwd = ChangeDirGuard::new(repo.path());
    let oldest = status_test::rev_parse(repo.path(), "HEAD~2");
    let boundary = status_test::rev_parse(repo.path(), "HEAD~1");
    let head = status_test::rev_parse(repo.path(), "HEAD");
    status_test::write_upstream_ref(&boundary).await;
    if shallow {
        fs::write(
            repo.path().join(".libra").join("shallow"),
            format!("{boundary}\n"),
        )
        .expect("write shallow");
        fs::remove_file(loose_object_path(repo.path(), &oldest)).expect("delete oldest");
    }
    drop(_cwd);
    (repo, boundary, head)
}

/// M-WALK2 X1–X3: merge-base, status, blame, describe, shortlog on a shallow repo.
#[tokio::test]
#[serial(cwd)]
async fn test_history_commands_on_shallow_repo_matrix() {
    let (repo, boundary, head) = diverged_shallow_fixture(true).await;
    let _cwd = ChangeDirGuard::new(repo.path());

    let merge_base = run_libra_command(&["merge-base", "HEAD", "origin/main"], repo.path());
    assert_cli_success(&merge_base, "X1 merge-base on shallow");
    assert_eq!(
        String::from_utf8_lossy(&merge_base.stdout).trim(),
        boundary,
        "X1 merge-base must be the shared shallow boundary"
    );

    let status = run_libra_command(&["status", "--short", "--branch"], repo.path());
    assert_cli_success(&status, "X2 status on shallow");
    let status_text = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_text.contains("[ahead 1]"),
        "X2 ahead/behind must count above the boundary: {status_text}"
    );
    assert!(
        !status_text.contains("cannot count"),
        "X2 must not warn about missing parents: {status_text}"
    );

    let blame = run_libra_command(&["blame", "c3.txt"], repo.path());
    assert_cli_success(&blame, "X3 blame on shallow");
    let blame_text = String::from_utf8_lossy(&blame.stdout);
    assert!(
        blame_text.contains(&head[..7]),
        "X3 blame must attribute the file: {blame_text}"
    );

    let describe = run_libra_command(&["describe", "--always"], repo.path());
    assert_cli_success(&describe, "X3 describe --always on shallow");
    let describe_text = String::from_utf8_lossy(&describe.stdout);
    assert!(
        describe_text.contains(&head[..7]),
        "X3 describe --always must print HEAD: {describe_text}"
    );

    let shortlog = run_libra_command(&["shortlog", "-s"], repo.path());
    assert_cli_success(&shortlog, "X3 shortlog -s on shallow");
    let shortlog_text = String::from_utf8_lossy(&shortlog.stdout);
    assert!(
        !shortlog_text.trim().is_empty(),
        "X3 shortlog must summarize reachable commits: {shortlog_text}"
    );
}

/// M-WALK2 X4: the same commands still work on a complete repository.
#[tokio::test]
#[serial(cwd)]
async fn test_history_commands_on_complete_repo_regression() {
    let (repo, boundary, head) = diverged_shallow_fixture(false).await;
    let _cwd = ChangeDirGuard::new(repo.path());

    let merge_base = run_libra_command(&["merge-base", "HEAD", "origin/main"], repo.path());
    assert_cli_success(&merge_base, "X4 merge-base on complete repo");
    assert_eq!(
        String::from_utf8_lossy(&merge_base.stdout).trim(),
        boundary,
        "X4 merge-base of HEAD and origin/main is the fork point"
    );

    let status = run_libra_command(&["status", "--short", "--branch"], repo.path());
    assert_cli_success(&status, "X4 status on complete repo");
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("[ahead 1]"),
        "X4 complete-repo ahead count"
    );

    assert_cli_success(
        &run_libra_command(&["blame", "c3.txt"], repo.path()),
        "X4 blame on complete repo",
    );
    let describe = run_libra_command(&["describe", "--always"], repo.path());
    assert_cli_success(&describe, "X4 describe on complete repo");
    assert!(
        String::from_utf8_lossy(&describe.stdout).contains(&head[..7]),
        "X4 describe still prints HEAD"
    );
    assert_cli_success(
        &run_libra_command(&["shortlog", "-s"], repo.path()),
        "X4 shortlog on complete repo",
    );
}
