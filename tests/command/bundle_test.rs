//! Integration tests for `libra bundle`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{fs, path::Path, process::Command};

use tempfile::tempdir;

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

fn advertised_head_names(repo: &Path, bundle: &Path) -> Vec<String> {
    let result = run_libra_command(&["bundle", "list-heads", bundle.to_str().unwrap()], repo);
    assert_eq!(
        result.status.code(),
        Some(0),
        "list-heads failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout)
        .lines()
        .filter_map(|line| line.split_once(' ').map(|(_, name)| name.to_string()))
        .collect()
}

fn advertised_head_pairs(repo: &Path, bundle: &Path) -> Vec<(String, String)> {
    let result = run_libra_command(&["bundle", "list-heads", bundle.to_str().unwrap()], repo);
    assert_eq!(result.status.code(), Some(0));
    String::from_utf8_lossy(&result.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once(' ')
                .map(|(oid, name)| (oid.to_string(), name.to_string()))
        })
        .collect()
}

fn repo_with_branch_and_tag() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["branch", "dev"], repo.path()),
        "create dev",
    );
    assert_cli_success(&run_libra_command(&["tag", "v1"], repo.path()), "tag v1");
    repo
}

#[test]
fn bundle_create_writes_a_v2_bundle() {
    let repo = create_committed_repo_via_cli();
    let path = repo.path().join("out.bundle");
    let result = run_libra_command(
        &["bundle", "create", path.to_str().unwrap(), "HEAD"],
        repo.path(),
    );
    assert_eq!(
        result.status.code(),
        Some(0),
        "bundle create failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&path).unwrap();
    assert!(
        bytes.starts_with(b"# v2 git bundle\n"),
        "missing v2 signature"
    );
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains(" HEAD\n") || text.lines().any(|line| line.ends_with(" HEAD")),
        "explicit HEAD must advertise a HEAD line: {text}"
    );
    // The pack follows the blank line that terminates the header.
    assert!(
        bytes.windows(6).any(|w| w == b"\n\nPACK"),
        "missing PACK after header"
    );
}

#[test]
fn bundle_list_heads_prints_refs() {
    let repo = create_committed_repo_via_cli();
    let path = repo.path().join("out.bundle");
    assert_eq!(
        run_libra_command(
            &["bundle", "create", path.to_str().unwrap(), "HEAD"],
            repo.path()
        )
        .status
        .code(),
        Some(0)
    );
    let result = run_libra_command(
        &["bundle", "list-heads", path.to_str().unwrap()],
        repo.path(),
    );
    assert_eq!(result.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&result.stdout).contains(" HEAD")
            || String::from_utf8_lossy(&result.stdout)
                .lines()
                .any(|line| line.ends_with(" HEAD")),
        "list-heads must print the HEAD advertisement: {}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[test]
fn bundle_verify_accepts_a_created_bundle() {
    let repo = create_committed_repo_via_cli();
    let path = repo.path().join("out.bundle");
    assert_eq!(
        run_libra_command(
            &["bundle", "create", path.to_str().unwrap(), "HEAD"],
            repo.path()
        )
        .status
        .code(),
        Some(0)
    );
    let result = run_libra_command(&["bundle", "verify", path.to_str().unwrap()], repo.path());
    assert_eq!(
        result.status.code(),
        Some(0),
        "verify failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("is okay"));
}

#[test]
fn bundle_verify_rejects_a_non_bundle() {
    let repo = create_committed_repo_via_cli();
    let path = repo.path().join("not.bundle");
    fs::write(&path, b"this is not a bundle\n").unwrap();
    let result = run_libra_command(&["bundle", "verify", path.to_str().unwrap()], repo.path());
    // An invalid bundle format is a verification failure (exit 1), like
    // `git bundle verify`; exit 128 is reserved for usage errors.
    assert_eq!(result.status.code(), Some(1));
}

#[test]
fn bundle_create_bad_rev_is_an_error() {
    let repo = create_committed_repo_via_cli();
    let path = repo.path().join("out.bundle");
    let result = run_libra_command(
        &["bundle", "create", path.to_str().unwrap(), "no-such-rev"],
        repo.path(),
    );
    assert_eq!(result.status.code(), Some(128));
    assert!(!path.exists(), "no half-written bundle should remain");
}

#[test]
fn bundle_outside_repository_is_an_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("out.bundle");
    let result = run_libra_command(
        &["bundle", "create", path.to_str().unwrap(), "HEAD"],
        dir.path(),
    );
    assert_eq!(result.status.code(), Some(128));
}

/// M-BCREATE C1–C5: `--all` / explicit `HEAD` advertise a `HEAD` line.
#[test]
fn test_bundle_create_advertises_head_matrix() {
    let repo = repo_with_branch_and_tag();
    let all = repo.path().join("all.bundle");
    assert_cli_success(
        &run_libra_command(
            &["bundle", "create", all.to_str().unwrap(), "--all"],
            repo.path(),
        ),
        "C1 bundle create --all",
    );
    let c1 = advertised_head_names(repo.path(), &all);
    for expected in ["HEAD", "refs/heads/main", "refs/heads/dev", "refs/tags/v1"] {
        assert!(
            c1.iter().any(|name| name == expected),
            "C1 missing {expected} in {c1:?}"
        );
    }

    let head_main = repo.path().join("head-main.bundle");
    assert_cli_success(
        &run_libra_command(
            &[
                "bundle",
                "create",
                head_main.to_str().unwrap(),
                "HEAD",
                "main",
            ],
            repo.path(),
        ),
        "C2 bundle create HEAD main",
    );
    let c2 = advertised_head_names(repo.path(), &head_main);
    assert!(
        c2.iter().any(|name| name == "HEAD"),
        "C2 missing HEAD: {c2:?}"
    );
    assert!(
        c2.iter().any(|name| name == "refs/heads/main"),
        "C2 missing refs/heads/main: {c2:?}"
    );

    assert_cli_success(
        &run_libra_command(&["switch", "--detach"], repo.path()),
        "C3 switch --detach",
    );
    let detached_oid =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo.path()).stdout)
            .trim()
            .to_string();
    let detached = repo.path().join("detached.bundle");
    assert_cli_success(
        &run_libra_command(
            &["bundle", "create", detached.to_str().unwrap(), "--all"],
            repo.path(),
        ),
        "C3 bundle create --all (detached)",
    );
    let c3 = advertised_head_pairs(repo.path(), &detached);
    let head_line = c3
        .iter()
        .find(|(_, name)| name == "HEAD")
        .expect("C3 must advertise HEAD");
    assert_eq!(
        head_line.0, detached_oid,
        "C3 HEAD must point at the detached commit"
    );

    let git_dest = repo.path().join("from-git");
    let git = Command::new("git")
        .args(["clone", all.to_str().unwrap(), git_dest.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", repo.path().join(".git-home"))
        .output()
        .expect("spawn git clone");
    assert!(
        git.status.success(),
        "C4 git clone bundle failed: {}{}",
        String::from_utf8_lossy(&git.stdout),
        String::from_utf8_lossy(&git.stderr)
    );
    let log = Command::new("git")
        .args(["-C", git_dest.to_str().unwrap(), "log", "--oneline"])
        .output()
        .expect("spawn git log");
    assert!(log.status.success(), "C4 git log failed");
    assert!(
        !String::from_utf8_lossy(&log.stdout).trim().is_empty(),
        "C4 git clone must check out a non-empty history"
    );

    let verify = run_libra_command(&["bundle", "verify", all.to_str().unwrap()], repo.path());
    assert_cli_success(&verify, "C5 verify");
    assert!(
        String::from_utf8_lossy(&verify.stdout).contains("is okay"),
        "C5 verify: {}",
        String::from_utf8_lossy(&verify.stdout)
    );
    assert_cli_success(
        &run_libra_command(&["bundle", "unbundle", all.to_str().unwrap()], repo.path()),
        "C5 unbundle",
    );
}
