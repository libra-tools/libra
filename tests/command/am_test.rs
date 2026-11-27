//! `am` on an unborn branch.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

use crate::command::{assert_cli_success, run_libra_command};

fn source_repo_with_commits(count: usize) -> tempfile::TempDir {
    let repo = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], repo.path()),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "A U Thor"], repo.path()),
        "name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "author@example.com"], repo.path()),
        "email",
    );
    for i in 0..count {
        let name = format!("file{i}.txt");
        fs::write(repo.path().join(&name), format!("content {i}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", &name], repo.path()), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("commit {i}")], repo.path()),
            "commit",
        );
    }
    repo
}

#[test]
#[serial(cwd)]
fn am_unborn_single_patch_creates_root_commit() {
    let source = source_repo_with_commits(1);
    let out = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(
            &[
                "format-patch",
                "-1",
                "-o",
                out.path().to_str().unwrap(),
                "HEAD",
            ],
            source.path(),
        ),
        "format-patch",
    );
    let patch = fs::read_dir(out.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let target = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], target.path()),
        "target init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "A U Thor"], target.path()),
        "target name",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "user.email", "author@example.com"],
            target.path(),
        ),
        "target email",
    );
    let am = run_libra_command(&["am", patch.to_str().unwrap()], target.path());
    assert_cli_success(&am, "am on unborn");
    let log = run_libra_command(&["log", "--format=%s"], target.path());
    let text = String::from_utf8_lossy(&log.stdout);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("commit 0"), "{text}");
    let parents = run_libra_command(&["log", "-1", "--format=%P"], target.path());
    assert!(String::from_utf8_lossy(&parents.stdout).trim().is_empty());
}

#[test]
#[serial(cwd)]
fn am_unborn_series_matches_git_history() {
    let source = source_repo_with_commits(2);
    let out = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(
            &[
                "format-patch",
                "-2",
                "-o",
                out.path().to_str().unwrap(),
                "HEAD",
            ],
            source.path(),
        ),
        "format-patch",
    );
    let mut patches: Vec<_> = fs::read_dir(out.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    patches.sort();
    let target = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], target.path()),
        "target init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "A U Thor"], target.path()),
        "target name",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "user.email", "author@example.com"],
            target.path(),
        ),
        "target email",
    );
    let mut args = vec!["am".to_string()];
    args.extend(
        patches
            .iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    assert_cli_success(&run_libra_command(&refs, target.path()), "am series");
    let log = run_libra_command(&["log", "--format=%s"], target.path());
    let text = String::from_utf8_lossy(&log.stdout);
    assert!(
        text.contains("commit 1") && text.contains("commit 0"),
        "{text}"
    );
}
