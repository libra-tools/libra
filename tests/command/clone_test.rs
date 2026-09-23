//! Tests clone command setup to ensure objects, refs, and working copies are created correctly.
//!
//! All tests in this file are **L2 (network)**: they require
//! `LIBRA_TEST_GITHUB_LIVE=1`, `LIBRA_TEST_GITHUB_TOKEN`, and
//! `LIBRA_TEST_GITHUB_NAMESPACE` to create and push to a temporary GitHub
//! repository. Without the explicit live-test flag and credentials, the tests
//! are skipped so normal acceptance runs do not depend on external GitHub state.

use std::{fs, process::Command, sync::OnceLock};

use libra::{command, command::clone::CloneArgs, internal::head::Head, utils::test};
use serial_test::serial;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// GitHub test-repo lifecycle helpers
// ---------------------------------------------------------------------------

struct GitHubTestRepo {
    full_name: String,
    https_url: String,
    token: String,
}

impl Drop for GitHubTestRepo {
    fn drop(&mut self) {
        // Safety: only delete repos whose name starts with "libra-test-"
        if !self.full_name.contains("/libra-test-") {
            return;
        }
        let _ = reqwest::blocking::Client::new()
            .delete(format!("https://api.github.com/repos/{}", self.full_name))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "libra-test")
            .header("Accept", "application/vnd.github+json")
            .send();
    }
}

static GITHUB_REPO: OnceLock<Option<GitHubTestRepo>> = OnceLock::new();
const LIVE_GITHUB_SKIP_MESSAGE: &str = "skipped (set LIBRA_TEST_GITHUB_LIVE=1, LIBRA_TEST_GITHUB_TOKEN, and LIBRA_TEST_GITHUB_NAMESPACE)";

/// Return whether the GitHub-backed clone tests should contact GitHub.
///
/// Test coverage: every clone scenario below flows through `github_test_repo`,
/// so the boundary between deterministic local acceptance and opt-in network
/// validation is exercised before any GitHub API call or authenticated push.
fn live_github_clone_tests_enabled() -> bool {
    std::env::var("LIBRA_TEST_GITHUB_LIVE")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

/// Get or lazily create the shared temporary GitHub repo.
/// Returns `None` (and tests skip) when the live flag or env vars are absent.
fn github_test_repo() -> Option<&'static GitHubTestRepo> {
    GITHUB_REPO
        .get_or_init(|| {
            if !live_github_clone_tests_enabled() {
                return None;
            }

            let token = std::env::var("LIBRA_TEST_GITHUB_TOKEN")
                .ok()
                .filter(|v| !v.is_empty())?;
            let namespace = std::env::var("LIBRA_TEST_GITHUB_NAMESPACE")
                .ok()
                .filter(|v| !v.is_empty())?;
            Some(setup_github_repo(&token, &namespace))
        })
        .as_ref()
}

/// Resolve the shared GitHub fixture from an async test without dropping
/// `reqwest::blocking` internals inside Tokio's worker runtime.
///
/// Test coverage: every `#[tokio::test]` in this file calls this helper before
/// invoking `clone::execute`; missing credentials still return `None` so the L2
/// network scenarios skip cleanly, while configured environments exercise the
/// real GitHub repository setup on a blocking thread.
async fn github_test_repo_for_async_test() -> Option<&'static GitHubTestRepo> {
    tokio::task::spawn_blocking(github_test_repo)
        .await
        .expect("GitHub test-repo setup task panicked")
}

fn setup_github_repo(token: &str, namespace: &str) -> GitHubTestRepo {
    let suffix = &uuid::Uuid::new_v4().to_string()[..6];
    let repo_name = format!("libra-test-{suffix}");
    let full_name = format!("{namespace}/{repo_name}");

    // Create repo via GitHub API
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.github.com/user/repos")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "libra-test")
        .header("Accept", "application/vnd.github+json")
        .json(&serde_json::json!({
            "name": repo_name,
            "auto_init": false,
            "private": false,
        }))
        .send()
        .expect("failed to create GitHub repo");
    assert!(
        resp.status().is_success(),
        "GitHub repo creation failed: {}",
        resp.text().unwrap_or_default()
    );

    let https_url = format!("https://github.com/{full_name}.git");

    // Push test data: main branch with a commit, then dev branch with another commit.
    let work_dir = tempfile::tempdir().expect("failed to create workdir for push");
    let wd = work_dir.path();

    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(wd)
            .args(args)
            .output()
            .expect("git command failed");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };

    let auth_url = format!("https://x-access-token:{token}@github.com/{full_name}.git");

    git(&["init"]);
    git(&["config", "user.name", "Libra Test"]);
    git(&["config", "user.email", "test@libra.dev"]);
    fs::write(wd.join("README.md"), "libra clone test repo").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "initial commit"]);

    // Detect the default branch name (may be main or master).
    let head_out = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let default_branch = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
    // Ensure we are on 'main'.
    if default_branch != "main" {
        git(&["branch", "-M", "main"]);
    }
    git(&["remote", "add", "origin", &auth_url]);
    git(&["push", "-u", "origin", "main"]);

    // Create dev branch with an extra commit.
    git(&["checkout", "-b", "dev"]);
    fs::write(wd.join("dev.txt"), "dev branch content").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "dev commit"]);
    git(&["push", "-u", "origin", "dev"]);

    GitHubTestRepo {
        full_name,
        https_url,
        token: token.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Clone tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial(cwd)]
async fn test_clone_branch() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(temp_path.path().to_str().unwrap().to_string()),
        branch: Some("dev".to_string()),
        single_branch: false,
        bare: false,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(temp_path.path().join(".libra").exists());
    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "dev"),
        _ => panic!("should be branch"),
    };
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_bare_repository() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());
    let repo_dir = temp_path.path().join("bare-clone.git");

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(repo_dir.to_str().unwrap().to_string()),
        branch: Some("dev".to_string()),
        single_branch: false,
        bare: true,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(
        repo_dir.join("libra.db").exists(),
        "bare clone should create libra.db at repo root"
    );
    assert!(
        repo_dir.join("info").join("exclude").exists(),
        "bare clone should create info/exclude"
    );
    assert!(
        repo_dir.join("objects").exists(),
        "bare clone should have objects directory"
    );
    assert!(
        !repo_dir.join(".libra").exists(),
        "bare clone should not create nested .libra"
    );

    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "dev"),
        _ => panic!("bare clone should still update HEAD to a branch"),
    };
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_branch_single_branch() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(temp_path.path().to_str().unwrap().to_string()),
        branch: Some("dev".to_string()),
        single_branch: true,
        bare: false,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(temp_path.path().join(".libra").exists());
    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "dev"),
        _ => panic!("should be branch"),
    };
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_default_branch() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(temp_path.path().to_str().unwrap().to_string()),
        branch: None,
        single_branch: false,
        bare: false,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(temp_path.path().join(".libra").exists());
    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "main"),
        _ => panic!("should be branch"),
    };
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_default_branch_single_branch() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(temp_path.path().to_str().unwrap().to_string()),
        branch: None,
        single_branch: true,
        bare: false,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(temp_path.path().join(".libra").exists());
    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "main"),
        _ => panic!("should be branch"),
    };
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_to_existing_empty_dir() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());
    let repo_path = temp_path.path().join("clone-target");
    fs::create_dir(&repo_path).unwrap();

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(repo_path.to_str().unwrap().to_string()),
        branch: Some("dev".to_string()),
        single_branch: false,
        bare: false,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(repo_path.join(".libra").exists());
    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "dev"),
        _ => panic!("should be branch"),
    };
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_to_existing_dir() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let repo_path = temp_path.path().join("clone-target");
    fs::create_dir(&repo_path).unwrap();
    let dummy_file = repo_path.join("exists.txt");
    fs::write(&dummy_file, "test").unwrap();

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(repo_path.to_str().unwrap().to_string()),
        branch: Some("dev".to_string()),
        single_branch: false,
        bare: false,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(!repo_path.join(".libra").exists());
    assert!(dummy_file.exists(), "pre-existing file should still exist");
    assert_eq!(fs::read_to_string(&dummy_file).unwrap(), "test");
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_to_dir_with_existing_file_name() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let conflict_path = temp_path.path().join("clone-target");
    fs::write(&conflict_path, "test").unwrap();

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(conflict_path.to_str().unwrap().to_string()),
        branch: Some("dev".to_string()),
        single_branch: false,
        bare: false,
        depth: None,
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(
        conflict_path.is_file(),
        "pre-existing file should remain a file"
    );
    assert_eq!(fs::read_to_string(&conflict_path).unwrap(), "test");
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_with_depth() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(temp_path.path().to_str().unwrap().to_string()),
        branch: None,
        single_branch: false,
        bare: false,
        depth: Some(1),
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(temp_path.path().join(".libra").exists());
    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "main"),
        _ => panic!("should be branch"),
    };
    // M-TEST T5: append T1 integrity assertions (live GitHub, depth 1).
    assert_depth_clone_integrity(temp_path.path(), "1", true);
}

#[tokio::test]
#[serial(cwd)]
async fn test_clone_with_depth_and_branch() {
    let repo = match github_test_repo_for_async_test().await {
        Some(r) => r,
        None => {
            eprintln!("{LIVE_GITHUB_SKIP_MESSAGE}");
            return;
        }
    };
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    command::clone::execute(CloneArgs {
        no_single_branch: false,
        origin: None,
        local: false,
        no_local: false,
        reject_shallow: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: None,
        shallow_since: None,
        shallow_exclude: vec![],
        deps_of: vec![],
        deps_depth_limit: None,
        no_checkout: false,
        no_progress: false,
        remote_repo: repo.https_url.clone(),
        local_path: Some(temp_path.path().to_str().unwrap().to_string()),
        branch: Some("dev".to_string()),
        single_branch: true,
        bare: false,
        depth: Some(5),
        tags: false,
        no_tags: false,
    })
    .await;

    assert!(temp_path.path().join(".libra").exists());
    match Head::current().await {
        Head::Branch(b) => assert_eq!(b, "dev"),
        _ => panic!("should be branch"),
    };
    // M-TEST T5: fixture `dev` has two commits, so depth 5 is untruncated.
    assert_depth_clone_integrity(temp_path.path(), "2", false);
}

/// M-TEST T1 integrity checks shared by live (T5) and local depth clones.
fn assert_depth_clone_integrity(
    repo: &std::path::Path,
    expected_count: &str,
    expect_shallow: bool,
) {
    use super::{assert_cli_success, run_libra_command};

    let count = run_libra_command(&["rev-list", "--count", "HEAD"], repo);
    assert_cli_success(&count, "rev-list --count HEAD");
    assert_eq!(
        String::from_utf8_lossy(&count.stdout).trim(),
        expected_count,
        "rev-list --count HEAD"
    );
    assert_cli_success(&run_libra_command(&["fsck"], repo), "fsck");
    assert_cli_success(&run_libra_command(&["log", "-1", "--oneline"], repo), "log");
    let shallow_flag = run_libra_command(&["rev-parse", "--is-shallow-repository"], repo);
    assert_cli_success(&shallow_flag, "rev-parse --is-shallow-repository");
    let is_shallow = String::from_utf8_lossy(&shallow_flag.stdout).trim() == "true";
    assert_eq!(
        is_shallow,
        expect_shallow,
        "shallow marker mismatch (file exists={})",
        repo.join(".libra").join("shallow").is_file()
    );
}

#[test]
fn clone_no_progress_flag_is_accepted() {
    let temp = tempdir().unwrap();
    // `--no-progress` parses and reaches the runtime (the fetch progress
    // suppression is covered by fetch's `apply_no_progress` unit test, which
    // clone reuses). With a bogus source it fails connecting, NOT at clap.
    let output = crate::command::run_libra_command(
        &["clone", "--no-progress", "/nonexistent/libra/repo", "dest"],
        temp.path(),
    );
    assert!(!output.status.success(), "clone of a bogus source fails");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--no-progress is accepted by the parser: {stderr}"
    );
}

#[test]
fn clone_no_single_branch_countermands_single_branch() {
    let temp = tempdir().unwrap();
    // `--single-branch --no-single-branch` (last wins) is NOT a clap conflict:
    // `--no-single-branch` countermands `--single-branch` via the symmetric
    // override, so it parses and fails later connecting to the bogus source,
    // not at clap. `--no-single-branch` (clone all branches) is the default.
    let output = crate::command::run_libra_command(
        &[
            "clone",
            "--single-branch",
            "--no-single-branch",
            "/nonexistent/libra/repo",
            "dest",
        ],
        temp.path(),
    );
    assert!(!output.status.success(), "clone of a bogus source fails");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("cannot be used with"),
        "--single-branch --no-single-branch parses (override, no conflict): {stderr}"
    );
}

#[test]
fn no_checkout_skips_working_tree() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    // Source repo with a committed `tracked.txt`, used as a local clone source.
    let src = create_committed_repo_via_cli();
    let work = tempdir().unwrap();

    // `clone --no-checkout` sets up .libra/refs/HEAD but does NOT populate the
    // working tree with the tracked file.
    let dst = work.path().join("dst");
    let out = crate::command::run_libra_command(
        &[
            "clone",
            "--no-checkout",
            src.path().to_str().unwrap(),
            dst.to_str().unwrap(),
        ],
        work.path(),
    );
    assert_cli_success(&out, "clone --no-checkout");
    assert!(dst.join(".libra").exists(), ".libra is created");
    assert!(
        !dst.join("tracked.txt").exists(),
        "--no-checkout leaves the tracked file unchecked-out"
    );
    let head = run_libra_command(&["rev-parse", "HEAD"], &dst);
    assert_cli_success(&head, "HEAD is resolvable after --no-checkout clone");

    // Control: a normal clone of the same source DOES check out the file.
    let dst2 = work.path().join("dst2");
    let out2 = run_libra_command(
        &[
            "clone",
            src.path().to_str().unwrap(),
            dst2.to_str().unwrap(),
        ],
        work.path(),
    );
    assert_cli_success(&out2, "normal clone");
    assert!(
        dst2.join("tracked.txt").exists(),
        "a normal clone checks out the tracked file"
    );
}

#[test]
fn origin_flag_names_the_remote() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let work = tempdir().unwrap();
    let dst = work.path().join("dst");

    // `clone -o upstream` names the remote `upstream` instead of `origin`.
    let out = run_libra_command(
        &[
            "clone",
            "-o",
            "upstream",
            src.path().to_str().unwrap(),
            dst.to_str().unwrap(),
        ],
        work.path(),
    );
    assert_cli_success(&out, "clone -o upstream");

    // The remote, branch tracking config, and remote-tracking ref all use the
    // chosen name; nothing is created under `origin`.
    let upstream_url = run_libra_command(&["config", "get", "remote.upstream.url"], &dst);
    assert_cli_success(&upstream_url, "remote.upstream.url is set");
    let origin_url = run_libra_command(&["config", "get", "remote.origin.url"], &dst);
    assert!(
        !origin_url.status.success(),
        "no remote.origin.url is created under -o upstream"
    );
    let branch_remote = run_libra_command(&["config", "get", "branch.main.remote"], &dst);
    assert_eq!(
        String::from_utf8_lossy(&branch_remote.stdout).trim(),
        "upstream",
        "branch.main.remote tracks the named remote"
    );
    let refs = run_libra_command(
        &["for-each-ref", "refs/remotes/", "--format=%(refname)"],
        &dst,
    );
    assert!(
        String::from_utf8_lossy(&refs.stdout).contains("refs/remotes/upstream/main"),
        "tracking ref uses the named remote"
    );

    // `-o <name> --no-tags` records the tag preference under the named remote,
    // not under `origin`.
    let dst2 = work.path().join("dst2");
    let out2 = run_libra_command(
        &[
            "clone",
            "-o",
            "upstream",
            "--no-tags",
            src.path().to_str().unwrap(),
            dst2.to_str().unwrap(),
        ],
        work.path(),
    );
    assert_cli_success(&out2, "clone -o upstream --no-tags");
    let tagopt = run_libra_command(&["config", "get", "remote.upstream.tagOpt"], &dst2);
    assert_eq!(
        String::from_utf8_lossy(&tagopt.stdout).trim(),
        "--no-tags",
        "tagOpt is recorded under the named remote"
    );
    let origin_tagopt = run_libra_command(&["config", "get", "remote.origin.tagOpt"], &dst2);
    assert!(
        !origin_tagopt.status.success(),
        "no remote.origin.tagOpt is created under -o upstream"
    );

    // Invalid remote names are usage errors (exit 129) and create no destination
    // (validation runs before touching the filesystem). Covers a name with a
    // space and a ref-format-invalid name (a `.lock` suffix) that has no
    // whitespace/control characters.
    for (idx, bad_name) in ["bad name", "feat.lock", "bad~name"].iter().enumerate() {
        let bad_dst = work.path().join(format!("bad{idx}"));
        let bad = run_libra_command(
            &[
                "clone",
                "-o",
                bad_name,
                src.path().to_str().unwrap(),
                bad_dst.to_str().unwrap(),
            ],
            work.path(),
        );
        assert_eq!(
            bad.status.code(),
            Some(129),
            "invalid -o name {bad_name:?} is rejected"
        );
        assert!(
            !bad_dst.exists(),
            "no destination is created for invalid -o name {bad_name:?}"
        );
    }
}

/// `--local` / `--no-local` / `-l` are accepted for Git compatibility and are
/// effectively no-ops: Libra's clone of a local-path source already reads its
/// objects directly. Cloning a local source succeeds with any of them.
#[test]
fn test_clone_local_flag_accepted_for_local_source() {
    use super::run_libra_command;

    let source = tempdir().expect("source dir");
    let sp = source.path();
    assert!(
        run_libra_command(&["init"], sp).status.success(),
        "init source"
    );
    run_libra_command(&["config", "set", "user.name", "t"], sp);
    run_libra_command(&["config", "set", "user.email", "t@t"], sp);
    fs::write(sp.join("f.txt"), "hello\n").expect("write f");
    assert!(
        run_libra_command(&["add", "f.txt"], sp).status.success(),
        "add"
    );
    assert!(
        run_libra_command(&["commit", "-m", "c1", "--no-verify"], sp)
            .status
            .success(),
        "commit"
    );
    let source_str = sp.to_str().unwrap();

    let dest_root = tempdir().expect("dest root");
    // Each flag form clones the local source successfully and gets the commit.
    for (idx, flag) in ["--local", "--no-local", "-l"].iter().enumerate() {
        let dest = dest_root.path().join(format!("clone{idx}"));
        let dest_str = dest.to_str().unwrap();
        let out = run_libra_command(&["clone", flag, source_str, dest_str], dest_root.path());
        assert!(
            out.status.success(),
            "clone {flag} should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(dest.join(".libra").exists(), "{flag}: clone created a repo");
        let log = run_libra_command(&["log", "--oneline"], &dest);
        assert!(
            String::from_utf8_lossy(&log.stdout).contains("c1"),
            "{flag}: cloned history present"
        );
    }

    // `--local --no-local` (mutually overriding) is accepted, last one wins.
    let dest = dest_root.path().join("clone-both");
    let out = run_libra_command(
        &[
            "clone",
            "--local",
            "--no-local",
            source_str,
            dest.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(
        out.status.success(),
        "clone --local --no-local should succeed"
    );
}

/// `--reject-shallow` allows a normal clone. Local Libra `--depth` is rejected
/// before fetch because that source cannot advertise shallow boundaries (D20 end state); the
/// pure post-fetch `clone_should_reject_shallow_only_for_unrequested_shallowness`
/// unit test covers the remaining remote-shallow decision.
#[test]
fn test_clone_reject_shallow_rejects_local_libra_depth() {
    use super::run_libra_command;

    let source = tempdir().expect("source dir");
    let sp = source.path();
    assert!(
        run_libra_command(&["init"], sp).status.success(),
        "init source"
    );
    run_libra_command(&["config", "set", "user.name", "t"], sp);
    run_libra_command(&["config", "set", "user.email", "t@t"], sp);
    for i in 0..3 {
        fs::write(sp.join("f.txt"), format!("v{i}\n")).expect("write f");
        assert!(
            run_libra_command(&["add", "f.txt"], sp).status.success(),
            "add"
        );
        assert!(
            run_libra_command(&["commit", "-m", &format!("c{i}"), "--no-verify"], sp)
                .status
                .success(),
            "commit c{i}"
        );
    }
    let source_str = sp.to_str().unwrap();

    let dest_root = tempdir().expect("dest root");
    // A full `--reject-shallow` clone of a non-shallow source succeeds.
    let full = dest_root.path().join("full");
    let out = run_libra_command(
        &[
            "clone",
            "--reject-shallow",
            source_str,
            full.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(
        out.status.success(),
        "--reject-shallow on a non-shallow source succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(full.join(".libra").exists(), "full clone created");

    // Local Libra sources cannot produce shallow boundary metadata (D20 end state), so
    // `--depth` must fail closed rather than leaving a missing-parent clone.
    let shallow = dest_root.path().join("shallow");
    let out = run_libra_command(
        &[
            "clone",
            "--reject-shallow",
            "--depth",
            "2",
            &format!("file://{source_str}"),
            shallow.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "--reject-shallow with local Libra --depth should fail closed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Error-Code: LBR-REPO-002"), "{stderr}");
    assert!(
        stderr.contains("local Libra remotes do not support --depth"),
        "{stderr}"
    );
    assert!(
        !shallow.join(".libra").exists(),
        "failed depth clone must not initialize target"
    );
}

/// `--reference`/`--shared`/`--reference-if-able`/`--dissociate` are accepted as
/// no-ops (Libra has no object alternates — it always copies). The clone still
/// succeeds; `--reference`/`--shared` add an explanatory warning, while
/// `--reference-if-able` (graceful) and `--dissociate` are silent.
#[test]
fn test_clone_object_alternates_flags_are_noops() {
    use super::run_libra_command;

    let source = tempdir().expect("source dir");
    let sp = source.path();
    assert!(
        run_libra_command(&["init"], sp).status.success(),
        "init source"
    );
    run_libra_command(&["config", "set", "user.name", "t"], sp);
    run_libra_command(&["config", "set", "user.email", "t@t"], sp);
    fs::write(sp.join("f.txt"), "x\n").expect("write f");
    assert!(
        run_libra_command(&["add", "f.txt"], sp).status.success(),
        "add"
    );
    assert!(
        run_libra_command(&["commit", "-m", "c1", "--no-verify"], sp)
            .status
            .success(),
        "commit"
    );
    let source_str = sp.to_str().unwrap();
    let dest_root = tempdir().expect("dest root");

    // --reference + --dissociate: clone succeeds, warning present for --reference.
    let d1 = dest_root.path().join("d1");
    let out = run_libra_command(
        &[
            "clone",
            "--reference",
            "/nonexistent/repo",
            "--dissociate",
            source_str,
            d1.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(
        out.status.success(),
        "clone with --reference/--dissociate succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(d1.join(".libra").exists(), "clone created");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--reference has no effect"),
        "--reference warns it is a no-op, got: {stderr}"
    );

    // --reference-if-able alone is silently ignored (graceful) — no warning.
    let d2 = dest_root.path().join("d2");
    let out = run_libra_command(
        &[
            "clone",
            "--reference-if-able",
            "/nonexistent/repo",
            source_str,
            d2.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(
        out.status.success(),
        "clone with --reference-if-able succeeds"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("have no effect"),
        "--reference-if-able is silently ignored (no warning)"
    );

    // -s/--shared now REGISTERS the source as an alternate for a LOCAL Libra
    // source (lore.md 2.11) — no longer a no-op. The clone succeeds and the
    // source becomes a protected shared store.
    let d3 = dest_root.path().join("d3");
    let out = run_libra_command(
        &["clone", "-s", source_str, d3.to_str().unwrap()],
        dest_root.path(),
    );
    assert!(out.status.success(), "clone with --shared succeeds");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("registered")
            && String::from_utf8_lossy(&out.stderr).contains("object alternate"),
        "--shared registers the alternate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let alts = run_libra_command(&["alternates", "list"], &d3);
    assert!(
        String::from_utf8_lossy(&alts.stdout).contains(".libra/objects"),
        "clone d3 borrows from the source"
    );
}

/// `clone --mirror` produces a bare repository whose `refs/heads/*` mirror all of
/// the fetched branches verbatim (no `refs/remotes/*` tracking refs), keeps tags,
/// and records the `remote.origin.mirror=true` marker (but NOT an inert
/// `+refs/*:refs/*` fetch refspec, which Libra's fetch would not honor).
#[test]
fn test_clone_mirror_maps_all_refs_and_sets_config() {
    use super::run_libra_command;

    let source = tempdir().expect("source dir");
    let sp = source.path();
    assert!(
        run_libra_command(&["init"], sp).status.success(),
        "init source"
    );
    run_libra_command(&["config", "set", "user.name", "t"], sp);
    run_libra_command(&["config", "set", "user.email", "t@t"], sp);
    fs::write(sp.join("f.txt"), "x\n").expect("write f");
    assert!(
        run_libra_command(&["add", "f.txt"], sp).status.success(),
        "add"
    );
    assert!(
        run_libra_command(&["commit", "-m", "c1", "--no-verify"], sp)
            .status
            .success(),
        "commit"
    );
    assert!(
        run_libra_command(&["branch", "feature"], sp)
            .status
            .success(),
        "branch"
    );
    assert!(
        run_libra_command(&["tag", "v1"], sp).status.success(),
        "tag"
    );
    let source_str = sp.to_str().unwrap();

    let dest_root = tempdir().expect("dest root");
    let mirror = dest_root.path().join("mirror");
    let out = run_libra_command(
        &["clone", "--mirror", source_str, mirror.to_str().unwrap()],
        dest_root.path(),
    );
    assert!(
        out.status.success(),
        "mirror clone succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let refs =
        String::from_utf8_lossy(&run_libra_command(&["show-ref"], &mirror).stdout).to_string();
    assert!(
        refs.contains("refs/heads/main"),
        "main mirrored to refs/heads: {refs}"
    );
    assert!(
        refs.contains("refs/heads/feature"),
        "feature mirrored to refs/heads: {refs}"
    );
    assert!(refs.contains("refs/tags/v1"), "tag kept: {refs}");
    assert!(
        !refs.contains("refs/remotes/"),
        "a mirror keeps no remote-tracking refs: {refs}"
    );

    let config = String::from_utf8_lossy(&run_libra_command(&["config", "--list"], &mirror).stdout)
        .to_string();
    assert!(
        config.contains("remote.origin.mirror=true"),
        "mirror marker config set: {config}"
    );
    assert!(
        config.contains("+refs/*:refs/*"),
        "mirror fetch refspec recorded: {config}"
    );
    // --mirror implies --bare: no working tree checked out.
    assert!(
        !mirror.join("f.txt").exists(),
        "mirror is bare (no working-tree checkout)"
    );
}

/// M-MIRROR (issues/474 CL-12): `--mirror` maps every advertised namespace
/// verbatim (`refs/heads`, `refs/tags`, `refs/notes`, `refs/mr`), records the
/// Git mirror config, and preserves a detached source HEAD (t5601:116-160).
#[test]
fn test_clone_mirror_all_namespaces_matrix() {
    use super::{assert_cli_success, create_gdeep_git_repo, git_success, run_libra_command};

    let gdeep = create_gdeep_git_repo();
    git_success(gdeep.dir.path(), &["config", "user.email", "t@t"]);
    git_success(gdeep.dir.path(), &["config", "user.name", "t"]);
    git_success(gdeep.dir.path(), &["notes", "add", "-m", "n1"]);

    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("mirror");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--mirror",
                gdeep.dir.path().to_str().unwrap(),
                dest.to_str().unwrap(),
            ],
            parent.path(),
        ),
        "M1 clone --mirror",
    );
    let refs = String::from_utf8_lossy(&run_libra_command(&["show-ref"], &dest).stdout).to_string();
    assert!(refs.contains("refs/heads/main"), "M1 heads: {refs}");
    assert!(refs.contains("refs/heads/dev"), "M1 dev: {refs}");
    assert!(refs.contains("refs/tags/v1"), "M1 tags: {refs}");
    assert!(refs.contains("refs/mr/1"), "M1 merge-request ref: {refs}");
    assert!(refs.contains("refs/notes/commits"), "M1 notes ref: {refs}");
    assert!(
        !refs.contains("refs/remotes/"),
        "M1 must not keep tracking refs: {refs}"
    );
    assert_eq!(
        refs.matches("refs/tags/v1").count(),
        1,
        "M3 tag must not be duplicated: {refs}"
    );

    let config = String::from_utf8_lossy(&run_libra_command(&["config", "--list"], &dest).stdout)
        .to_string();
    assert!(
        config.contains("remote.origin.mirror=true"),
        "M2 mirror marker: {config}"
    );
    assert!(
        config.contains("+refs/*:refs/*"),
        "M2 mirror fetch refspec: {config}"
    );

    let detached_oid = super::git_rev_parse(gdeep.dir.path(), "HEAD");
    git_success(gdeep.dir.path(), &["checkout", "--detach"]);
    let dest2 = parent.path().join("detached");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--mirror",
                gdeep.dir.path().to_str().unwrap(),
                dest2.to_str().unwrap(),
            ],
            parent.path(),
        ),
        "M4 clone detached mirror",
    );
    let head = run_libra_command(&["rev-parse", "HEAD"], &dest2);
    assert_cli_success(&head, "M4 rev-parse HEAD");
    let got = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_eq!(
        got, detached_oid,
        "M4 HEAD must stay on the detached commit"
    );
}

/// `--filter`/`--shallow-since`/`--shallow-exclude` are accepted but ignored
/// (Libra has no partial-clone/promisor support and its fetch only does `--depth`
/// shallow), so a COMPLETE clone is performed and each given flag emits a warning.
#[test]
fn test_clone_unsupported_fetch_optimizations_warn_and_full_clone() {
    use super::run_libra_command;

    let source = tempdir().expect("source dir");
    let sp = source.path();
    assert!(
        run_libra_command(&["init"], sp).status.success(),
        "init source"
    );
    run_libra_command(&["config", "set", "user.name", "t"], sp);
    run_libra_command(&["config", "set", "user.email", "t@t"], sp);
    fs::write(sp.join("f.txt"), "x\n").expect("write f");
    assert!(
        run_libra_command(&["add", "f.txt"], sp).status.success(),
        "add"
    );
    assert!(
        run_libra_command(&["commit", "-m", "c1", "--no-verify"], sp)
            .status
            .success(),
        "commit"
    );
    let source_str = sp.to_str().unwrap();
    let dest_root = tempdir().expect("dest root");

    // --filter: full clone (the blob is present) + a warning.
    let d1 = dest_root.path().join("d1");
    let out = run_libra_command(
        &[
            "clone",
            "--filter",
            "blob:none",
            source_str,
            d1.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(
        out.status.success(),
        "clone with --filter succeeds (full clone): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        d1.join("f.txt").exists(),
        "a full clone is performed (the filtered-out blob is still present)"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--filter is ignored"),
        "--filter warns it is ignored"
    );

    // --shallow-since + --shallow-exclude (multi): full clone + warnings.
    let d2 = dest_root.path().join("d2");
    let out = run_libra_command(
        &[
            "clone",
            "--shallow-since",
            "2020-01-01",
            "--shallow-exclude",
            "v1",
            "--shallow-exclude",
            "v2",
            source_str,
            d2.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(out.status.success(), "clone with --shallow-* succeeds");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--shallow-since is ignored"),
        "--shallow-since warns"
    );
    assert!(
        stderr.contains("--shallow-exclude is ignored"),
        "--shallow-exclude warns"
    );
}

/// A Git clone reports the fetch transfer counts `objects_fetched` and
/// `bytes_received` in its `--json` output (both > 0 for a non-empty source).
#[test]
fn test_clone_json_reports_fetch_transfer_counts() {
    use super::run_libra_command;

    let source = tempdir().expect("source dir");
    let sp = source.path();
    assert!(
        run_libra_command(&["init"], sp).status.success(),
        "init source"
    );
    run_libra_command(&["config", "set", "user.name", "t"], sp);
    run_libra_command(&["config", "set", "user.email", "t@t"], sp);
    fs::write(sp.join("f.txt"), "hello\n").expect("write f");
    assert!(
        run_libra_command(&["add", "f.txt"], sp).status.success(),
        "add"
    );
    assert!(
        run_libra_command(&["commit", "-m", "c1", "--no-verify"], sp)
            .status
            .success(),
        "commit"
    );
    let source_str = sp.to_str().unwrap();

    let dest_root = tempdir().expect("dest root");
    let dest = dest_root.path().join("clone");
    let out = run_libra_command(
        &["clone", "--json", source_str, dest.to_str().unwrap()],
        dest_root.path(),
    );
    assert!(
        out.status.success(),
        "clone --json should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("clone --json emits valid JSON");
    let data = &json["data"];
    let objects = data["objects_fetched"]
        .as_u64()
        .expect("objects_fetched is present and numeric");
    let bytes = data["bytes_received"]
        .as_u64()
        .expect("bytes_received is present and numeric");
    assert!(
        objects > 0,
        "a non-empty source transfers objects: {objects}"
    );
    assert!(bytes > 0, "a non-empty source transfers bytes: {bytes}");
}

/// FM-01 (M-MAT T2): cloning a Libra source materializes executable entries
/// with their execute bit and keeps ordinary files non-executable.
#[cfg(unix)]
#[test]
fn test_clone_preserves_executable_bit_from_libra_source() {
    use std::os::unix::fs::PermissionsExt;

    use super::run_libra_command;

    let source = tempdir().expect("source dir");
    let source_path = source.path();
    assert!(
        run_libra_command(&["init"], source_path).status.success(),
        "init source"
    );
    run_libra_command(&["config", "set", "user.name", "t"], source_path);
    run_libra_command(&["config", "set", "user.email", "t@t"], source_path);
    let script = source_path.join("run.sh");
    fs::write(&script, "#!/bin/sh\necho run\n").expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod script");
    fs::write(source_path.join("plain.txt"), "plain\n").expect("write plain");
    assert!(
        run_libra_command(&["add", "run.sh", "plain.txt"], source_path)
            .status
            .success(),
        "stage files"
    );
    assert!(
        run_libra_command(&["commit", "-m", "modes", "--no-verify"], source_path)
            .status
            .success(),
        "commit files"
    );

    let dest_root = tempdir().expect("dest root");
    let dest = dest_root.path().join("clone");
    let output = run_libra_command(
        &[
            "clone",
            source_path.to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(
        output.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::symlink_metadata(dest.join("run.sh"))
            .expect("cloned script metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "cloned 100755 entry must be executable"
    );
    assert_eq!(
        fs::symlink_metadata(dest.join("plain.txt"))
            .expect("cloned plain metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "cloned 100644 entry must not be executable"
    );
}

/// M-BOUND: local Git `file://` / `--no-local` shallow clone writes the Git
/// shortest-distance union boundary. `--single-branch --no-tags` isolates the
/// walk from CL-05 branch/tag-range work.
#[test]
fn test_clone_depth_from_local_git_source_boundary_matrix() {
    use super::{
        assert_cli_success, create_gdeep_git_repo, create_linear_git_repo, git_success,
        read_shallow_oids, run_libra_command,
    };

    let (linear, linear_oids) = create_linear_git_repo(3);
    let c1 = &linear_oids[0];
    let c2 = &linear_oids[1];
    let c3 = &linear_oids[2];
    let linear_url = format!("file://{}", linear.path().display());
    let gdeep = create_gdeep_git_repo();
    git_success(gdeep.dir.path(), &["update-ref", "-d", "refs/mr/1"]);
    let gdeep_url = format!("file://{}", gdeep.dir.path().display());
    let dest_root = tempdir().expect("dest root");

    let b1 = dest_root.path().join("b1");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            "--single-branch",
            "--no-tags",
            &linear_url,
            b1.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "B1 clone --depth 1");
    assert_eq!(read_shallow_oids(&b1), vec![c3.clone()], "B1 shallow");
    let count = run_libra_command(&["rev-list", "--count", "HEAD"], &b1);
    assert_cli_success(&count, "B1 rev-list");
    assert_eq!(
        String::from_utf8_lossy(&count.stdout).trim(),
        "1",
        "B1 count"
    );

    let b2 = dest_root.path().join("b2");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            "--no-single-branch",
            &gdeep_url,
            b2.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "B2 clone --no-single-branch");
    let mut expected_b2 = vec![gdeep.c1.clone(), gdeep.c3.clone(), gdeep.dev1.clone()];
    expected_b2.sort();
    assert_eq!(read_shallow_oids(&b2), expected_b2, "B2 shallow");

    let b3 = dest_root.path().join("b3");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "2",
            "--single-branch",
            "--no-tags",
            &linear_url,
            b3.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "B3 clone --depth 2");
    assert_eq!(read_shallow_oids(&b3), vec![c2.clone()], "B3 shallow");
    let count = run_libra_command(&["rev-list", "--count", "HEAD"], &b3);
    assert_cli_success(&count, "B3 rev-list");
    assert_eq!(
        String::from_utf8_lossy(&count.stdout).trim(),
        "2",
        "B3 count"
    );

    let b3_path = dest_root.path().join("b3-path");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "2",
            "--no-local",
            "--single-branch",
            "--no-tags",
            linear.path().to_str().unwrap(),
            b3_path.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "B3 --no-local path");
    assert_eq!(
        read_shallow_oids(&b3_path),
        vec![c2.clone()],
        "B3 --no-local shallow"
    );

    let b5 = dest_root.path().join("b5");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "10",
            "--single-branch",
            "--no-tags",
            &linear_url,
            b5.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "B5 depth past history");
    assert!(
        read_shallow_oids(&b5).is_empty(),
        "B5 must not write .libra/shallow"
    );
    let count = run_libra_command(&["rev-list", "--count", "HEAD"], &b5);
    assert_cli_success(&count, "B5 rev-list");
    assert_eq!(
        String::from_utf8_lossy(&count.stdout).trim(),
        "3",
        "B5 full history"
    );

    let root_src = tempdir().expect("root source");
    git_success(root_src.path(), &["init", "-b", "main"]);
    git_success(root_src.path(), &["config", "user.name", "Root"]);
    git_success(root_src.path(), &["config", "user.email", "root@test"]);
    git_success(root_src.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(root_src.path().join("only.txt"), "root\n").expect("write root");
    git_success(root_src.path(), &["add", "only.txt"]);
    git_success(root_src.path(), &["commit", "-m", "root"]);
    let root_oid =
        String::from_utf8_lossy(&super::git_output(root_src.path(), &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
    let b4 = dest_root.path().join("b4");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            "--single-branch",
            "--no-tags",
            &format!("file://{}", root_src.path().display()),
            b4.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "B4 single-root clone");
    assert_eq!(read_shallow_oids(&b4), vec![root_oid], "B4 shallow");

    let objects = run_libra_command(&["rev-list", "--objects", "--all"], &b1);
    assert_cli_success(&objects, "B6 objects");
    let object_text = String::from_utf8_lossy(&objects.stdout);
    assert!(
        !object_text.contains(c2),
        "B6 must not contain c2: {object_text}"
    );
    assert!(
        !object_text.contains(c1),
        "B6 must not contain c1: {object_text}"
    );
}

/// M-SCOPE: `--depth` implies `--single-branch` (S1), explicit `--single-branch`
/// (S2), `--no-single-branch` (S3), `-b` (S4), and a later source-only fetch
/// updates FETCH_HEAD only (S5).
#[test]
fn test_clone_depth_implies_single_branch_matrix() {
    use super::{assert_cli_success, create_gdeep_git_repo, run_libra_command};

    let gdeep = create_gdeep_git_repo();
    let url = format!("file://{}", gdeep.dir.path().display());
    let dest_root = tempdir().expect("scope dest root");

    let remote_refs = |repo: &std::path::Path| -> String {
        let out = run_libra_command(
            &["for-each-ref", "--format=%(refname)", "refs/remotes"],
            repo,
        );
        assert_cli_success(&out, "for-each-ref remotes");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let fetch_spec = |repo: &std::path::Path| -> String {
        let out = run_libra_command(&["config", "get", "remote.origin.fetch"], repo);
        assert_cli_success(&out, "config get remote.origin.fetch");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    let s1 = dest_root.path().join("s1");
    let out = run_libra_command(
        &["clone", "--depth", "1", &url, s1.to_str().unwrap()],
        dest_root.path(),
    );
    assert_cli_success(&out, "S1 clone --depth 1");
    let s1_refs = remote_refs(&s1);
    assert!(
        s1_refs.contains("refs/remotes/origin/main"),
        "S1 must have origin/main: {s1_refs}"
    );
    assert!(
        !s1_refs.contains("refs/remotes/origin/dev"),
        "S1 must not have origin/dev: {s1_refs}"
    );
    assert!(
        !s1_refs.contains("refs/remotes/origin/mr/"),
        "S1 must not have origin/mr: {s1_refs}"
    );
    assert_eq!(
        fetch_spec(&s1),
        "+refs/heads/main:refs/remotes/origin/main",
        "S1 fetch refspec"
    );

    let s2 = dest_root.path().join("s2");
    let out = run_libra_command(
        &[
            "clone",
            "--single-branch",
            gdeep.dir.path().to_str().unwrap(),
            s2.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "S2 clone --single-branch path");
    let s2_refs = remote_refs(&s2);
    assert!(
        s2_refs.contains("refs/remotes/origin/main"),
        "S2 must have origin/main: {s2_refs}"
    );
    assert!(
        !s2_refs.contains("refs/remotes/origin/dev"),
        "S2 must not have origin/dev: {s2_refs}"
    );
    assert_eq!(
        fetch_spec(&s2),
        "+refs/heads/main:refs/remotes/origin/main",
        "S2 fetch refspec"
    );

    let s3 = dest_root.path().join("s3");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            "--no-single-branch",
            &url,
            s3.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "S3 clone --no-single-branch");
    let s3_refs = remote_refs(&s3);
    assert!(
        s3_refs.contains("refs/remotes/origin/main"),
        "S3 must have origin/main: {s3_refs}"
    );
    assert!(
        s3_refs.contains("refs/remotes/origin/dev"),
        "S3 must have origin/dev: {s3_refs}"
    );

    let s4 = dest_root.path().join("s4");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            "-b",
            "dev",
            &url,
            s4.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "S4 clone -b dev");
    let s4_refs = remote_refs(&s4);
    assert!(
        s4_refs.contains("refs/remotes/origin/dev"),
        "S4 must have origin/dev: {s4_refs}"
    );
    assert!(
        !s4_refs.contains("refs/remotes/origin/main"),
        "S4 must not have origin/main: {s4_refs}"
    );
    assert_eq!(
        fetch_spec(&s4),
        "+refs/heads/dev:refs/remotes/origin/dev",
        "S4 fetch refspec"
    );
    let head = run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &s4);
    assert_cli_success(&head, "S4 HEAD");
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "dev",
        "S4 checks out dev"
    );

    let out = run_libra_command(&["fetch", "origin", "dev"], &s1);
    assert_cli_success(&out, "S5 fetch origin dev");
    let s5_refs = remote_refs(&s1);
    assert!(
        !s5_refs.contains("refs/remotes/origin/dev"),
        "S5 must not create origin/dev: {s5_refs}"
    );
    let fetch_head = fs::read_to_string(s1.join(".libra/FETCH_HEAD")).expect("S5 FETCH_HEAD");
    assert!(
        fetch_head.contains("branch 'dev'"),
        "S5 FETCH_HEAD must record dev: {fetch_head}"
    );
}

/// M-LOCAL: a plain local Git path ignores `--depth` / `--shallow-*` / `--filter`
/// with Git's warning text; `file://` and `--no-local` keep transport shallow;
/// local Libra sources stay D20.
#[test]
fn test_clone_local_path_ignores_shallow_options_matrix() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, create_linear_git_repo,
        read_shallow_oids, run_libra_command,
    };

    let (linear, _oids) = create_linear_git_repo(3);
    let src = linear.path();
    let dest_root = tempdir().expect("local-clone dest");

    let l1 = dest_root.path().join("l1");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            src.to_str().unwrap(),
            l1.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "L1 plain path --depth 1");
    let l1_err = String::from_utf8_lossy(&out.stderr);
    assert!(
        l1_err.contains("warning: --depth is ignored in local clones; use file:// instead."),
        "L1 Git warning: {l1_err}"
    );
    assert!(
        read_shallow_oids(&l1).is_empty(),
        "L1 must not write shallow"
    );
    let count = run_libra_command(&["rev-list", "--count", "HEAD"], &l1);
    assert_cli_success(&count, "L1 rev-list");
    assert_eq!(
        String::from_utf8_lossy(&count.stdout).trim(),
        "3",
        "L1 complete history"
    );

    let l2 = dest_root.path().join("l2");
    let out = run_libra_command(
        &[
            "clone",
            "--shallow-since",
            "2000-01-01",
            "--shallow-exclude",
            "main",
            "--filter",
            "blob:none",
            src.to_str().unwrap(),
            l2.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "L2 ignored shallow opts");
    let l2_err = String::from_utf8_lossy(&out.stderr);
    assert!(
        l2_err
            .contains("warning: --shallow-since is ignored in local clones; use file:// instead."),
        "L2 since warning: {l2_err}"
    );
    assert!(
        l2_err.contains(
            "warning: --shallow-exclude is ignored in local clones; use file:// instead."
        ),
        "L2 exclude warning: {l2_err}"
    );
    assert!(
        l2_err.contains("warning: --filter is ignored in local clones; use file:// instead."),
        "L2 filter warning: {l2_err}"
    );
    assert!(
        read_shallow_oids(&l2).is_empty(),
        "L2 must not write shallow"
    );

    let l3 = dest_root.path().join("l3");
    let out = run_libra_command(
        &[
            "clone",
            "--no-local",
            "--depth",
            "2",
            src.to_str().unwrap(),
            l3.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "L3 --no-local --depth 2");
    assert_eq!(
        read_shallow_oids(&l3).len(),
        1,
        "L3 transport shallow boundary"
    );
    let count = run_libra_command(&["rev-list", "--count", "HEAD"], &l3);
    assert_cli_success(&count, "L3 rev-list");
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2", "L3 B3");

    let l3l = dest_root.path().join("l3l");
    let out = run_libra_command(
        &[
            "clone",
            "-l",
            "--depth",
            "1",
            src.to_str().unwrap(),
            l3l.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "L3 -l --depth 1");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("warning: --depth is ignored in local clones; use file:// instead."),
        "L3 -l warning"
    );
    assert!(read_shallow_oids(&l3l).is_empty(), "L3 -l complete clone");

    let libra_src = create_committed_repo_via_cli();
    let l4_path = dest_root.path().join("l4-path");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            libra_src.path().to_str().unwrap(),
            l4_path.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(
        !out.status.success(),
        "L4 plain Libra --depth must fail closed"
    );
    let l4_err = String::from_utf8_lossy(&out.stderr);
    assert!(l4_err.contains("LBR-REPO-002"), "L4 path D20: {l4_err}");
    assert!(
        !l4_path.join(".libra").exists(),
        "L4 must not leave a target"
    );

    let l4_file = dest_root.path().join("l4-file");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            &format!("file://{}", libra_src.path().display()),
            l4_file.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(!out.status.success(), "L4 file:// Libra --depth must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-REPO-002"),
        "L4 file:// D20"
    );

    let l5j = dest_root.path().join("l5-json");
    let out = run_libra_command(
        &[
            "--json",
            "clone",
            "--depth",
            "1",
            src.to_str().unwrap(),
            l5j.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "L5 --json");
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(
        json.contains("--depth is ignored in local clones; use file:// instead."),
        "L5 JSON warnings: {json}"
    );

    let l5q = dest_root.path().join("l5-quiet");
    let out = run_libra_command(
        &[
            "--quiet",
            "clone",
            "--depth",
            "1",
            src.to_str().unwrap(),
            l5q.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "L5 --quiet");
    let quiet_err = String::from_utf8_lossy(&out.stderr);
    assert!(
        quiet_err.contains("warning: --depth is ignored in local clones; use file:// instead."),
        "L5 quiet still warns: {quiet_err}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("Cloned into"),
        "L5 quiet suppresses the success summary"
    );
}

/// M-SSRC: clone a Git-generated shallow source, reject-shallow, and corrupt
/// source metadata.
#[test]
fn test_clone_from_shallow_git_source_matrix() {
    use super::{
        assert_cli_success, create_linear_git_repo, git_success, read_shallow_oids,
        run_libra_command,
    };

    let (linear, oids) = create_linear_git_repo(3);
    let c3 = &oids[2];
    let dest_root = tempdir().expect("ssrc dest");
    let gshallow = dest_root.path().join("gshallow");
    git_success(
        dest_root.path(),
        &[
            "clone",
            "--depth",
            "1",
            "--single-branch",
            "--no-tags",
            &format!("file://{}", linear.path().display()),
            gshallow.to_str().unwrap(),
        ],
    );
    assert!(
        gshallow.join(".git").join("shallow").is_file(),
        "git must produce a shallow source"
    );

    let r1_path = dest_root.path().join("r1-path");
    let out = run_libra_command(
        &[
            "clone",
            gshallow.to_str().unwrap(),
            r1_path.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "R1 clone plain shallow path");
    assert_eq!(
        read_shallow_oids(&r1_path),
        vec![c3.clone()],
        "R1 path shallow"
    );
    assert_cli_success(
        &run_libra_command(&["log", "--oneline"], &r1_path),
        "R1 path log",
    );
    assert_cli_success(&run_libra_command(&["fsck"], &r1_path), "R1 path fsck");

    let r1_file = dest_root.path().join("r1-file");
    let out = run_libra_command(
        &[
            "clone",
            &format!("file://{}", gshallow.display()),
            r1_file.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "R1 clone file:// shallow");
    assert_eq!(
        read_shallow_oids(&r1_file),
        vec![c3.clone()],
        "R1 file shallow"
    );

    let r2 = dest_root.path().join("r2");
    let out = run_libra_command(
        &[
            "clone",
            "--reject-shallow",
            gshallow.to_str().unwrap(),
            r2.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(!out.status.success(), "R2 reject-shallow path");
    assert_eq!(out.status.code(), Some(128));
    let r2_err = String::from_utf8_lossy(&out.stderr);
    assert!(
        r2_err.contains("source repository is shallow, reject to clone."),
        "R2 Git text: {r2_err}"
    );
    assert!(!r2.join(".libra").exists(), "R2 must not leave a target");

    let r2_nl = dest_root.path().join("r2-nl");
    let out = run_libra_command(
        &[
            "clone",
            "--reject-shallow",
            "--no-local",
            gshallow.to_str().unwrap(),
            r2_nl.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(!out.status.success(), "R2 reject-shallow --no-local");
    assert!(!r2_nl.join(".libra").exists());

    let r3 = dest_root.path().join("r3");
    let out = run_libra_command(
        &[
            "clone",
            "--reject-shallow",
            linear.path().to_str().unwrap(),
            r3.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "R3 reject-shallow complete source");

    let r4 = dest_root.path().join("r4");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            &format!("file://{}", gshallow.display()),
            r4.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "R4 --depth 1 file:// shallow");
    assert_eq!(read_shallow_oids(&r4), vec![c3.clone()], "R4 boundary");
    assert_cli_success(&run_libra_command(&["log", "--oneline"], &r4), "R4 log");
    assert_cli_success(&run_libra_command(&["fsck"], &r4), "R4 fsck");
}

#[test]
fn test_clone_reject_shallow_local_git_source() {
    use super::{create_linear_git_repo, git_success, run_libra_command};

    let (linear, _) = create_linear_git_repo(2);
    let dest_root = tempdir().expect("r5 dest");
    let gshallow = dest_root.path().join("gshallow");
    git_success(
        dest_root.path(),
        &[
            "clone",
            "--depth",
            "1",
            &format!("file://{}", linear.path().display()),
            gshallow.to_str().unwrap(),
        ],
    );
    fs::write(gshallow.join(".git").join("shallow"), "not-an-oid\n").expect("corrupt shallow");

    let r5 = dest_root.path().join("r5");
    let out = run_libra_command(
        &[
            "clone",
            &format!("file://{}", gshallow.display()),
            r5.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert!(!out.status.success(), "R5 corrupt source shallow");
    assert_eq!(out.status.code(), Some(128));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("LBR-REPO-002"),
        "R5 must be a repository error: {err}"
    );
    assert!(!r5.join(".libra").exists(), "R5 must not leave a target");
}

/// M-BUNDLE U1–U8: clone from a Libra-created Git v2 bundle.
#[test]
fn test_clone_from_bundle_matrix() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["branch", "dev"], src.path()),
        "branch dev",
    );
    assert_cli_success(&run_libra_command(&["tag", "v1"], src.path()), "tag v1");
    let parent = tempfile::tempdir().unwrap();

    let b1 = parent.path().join("b1.bundle");
    assert_cli_success(
        &run_libra_command(
            &["bundle", "create", b1.to_str().unwrap(), "--all"],
            src.path(),
        ),
        "U1 create --all",
    );
    let u1 = run_libra_command(&["clone", b1.to_str().unwrap()], parent.path());
    assert_cli_success(&u1, "U1 clone b1.bundle");
    let u1_dest = parent.path().join("b1");
    assert!(u1_dest.join(".libra").exists(), "U1 dest drops .bundle");
    assert!(u1_dest.join("tracked.txt").exists(), "U1 checks out HEAD");

    let nested = parent.path().join("dir");
    fs::create_dir_all(&nested).unwrap();
    let b3 = nested.join("b3.bundle");
    fs::copy(&b1, &b3).unwrap();
    let u1b = run_libra_command(
        &["clone", nested.join("b3").to_str().unwrap()],
        parent.path(),
    );
    assert_cli_success(&u1b, "U1 clone dir/b3");
    assert!(parent.path().join("b3").join(".libra").exists());

    let missing = parent.path().join("b4.bundle");
    let u2 = run_libra_command(&["clone", missing.to_str().unwrap()], parent.path());
    assert!(!u2.status.success(), "U2 missing bundle");
    let u2_err = format!(
        "{}{}",
        String::from_utf8_lossy(&u2.stdout),
        String::from_utf8_lossy(&u2.stderr)
    );
    assert!(
        u2_err.contains("does not exist"),
        "U2 must say repository does not exist: {u2_err}"
    );

    let main_only = parent.path().join("main-only.bundle");
    assert_cli_success(
        &run_libra_command(
            &["bundle", "create", main_only.to_str().unwrap(), "main"],
            src.path(),
        ),
        "U3 create main",
    );
    let u3_dest = parent.path().join("u3");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                main_only.to_str().unwrap(),
                u3_dest.to_str().unwrap(),
            ],
            parent.path(),
        ),
        "U3 clone without HEAD",
    );
    assert!(u3_dest.join("tracked.txt").exists(), "U3 checks out main");

    let dev_only = parent.path().join("dev-only.bundle");
    assert_cli_success(
        &run_libra_command(
            &["bundle", "create", dev_only.to_str().unwrap(), "dev"],
            src.path(),
        ),
        "U4 create dev",
    );
    let u4_dest = parent.path().join("u4");
    let u4 = run_libra_command(
        &[
            "clone",
            dev_only.to_str().unwrap(),
            u4_dest.to_str().unwrap(),
        ],
        parent.path(),
    );
    assert_cli_success(&u4, "U4 clone without default branch");
    assert!(
        !u4_dest.join("tracked.txt").exists(),
        "U4 must not check out a local branch"
    );

    let bare_dest = parent.path().join("bare-from-bundle");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--bare",
                b1.to_str().unwrap(),
                bare_dest.to_str().unwrap(),
            ],
            parent.path(),
        ),
        "U5 clone --bare",
    );
    assert!(bare_dest.join("libra.db").exists(), "U5 bare dest");
    assert!(
        !bare_dest.join("tracked.txt").exists(),
        "U5 bare has no worktree"
    );

    let depth_dest = parent.path().join("depth-from-bundle");
    let depth = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            b1.to_str().unwrap(),
            depth_dest.to_str().unwrap(),
        ],
        parent.path(),
    );
    assert_cli_success(&depth, "U5 clone --depth 1");
    let depth_text = format!(
        "{}{}",
        String::from_utf8_lossy(&depth.stdout),
        String::from_utf8_lossy(&depth.stderr)
    );
    assert!(
        depth_text.contains("--depth is ignored in local clones"),
        "U5 must warn that --depth is ignored: {depth_text}"
    );
    assert!(depth_dest.join("tracked.txt").exists());

    let incremental = parent.path().join("incremental.bundle");
    let bytes = fs::read(&main_only).unwrap();
    let header_end = bytes.windows(2).position(|w| w == b"\n\n").unwrap();
    let mut inc = Vec::from(&bytes[..header_end + 1]);
    inc.extend_from_slice(b"-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa missing\n");
    inc.extend_from_slice(&bytes[header_end + 1..]);
    fs::write(&incremental, inc).unwrap();
    let u6_dest = parent.path().join("u6");
    let u6 = run_libra_command(
        &[
            "clone",
            incremental.to_str().unwrap(),
            u6_dest.to_str().unwrap(),
        ],
        parent.path(),
    );
    assert!(!u6.status.success(), "U6 missing prerequisite");
    assert!(
        !u6_dest.join(".libra").exists(),
        "U6 must not leave a target"
    );

    let corrupt = parent.path().join("corrupt.bundle");
    fs::write(&corrupt, b"this is not a bundle\n").unwrap();
    let u7_dest = parent.path().join("u7");
    let u7 = run_libra_command(
        &[
            "clone",
            corrupt.to_str().unwrap(),
            u7_dest.to_str().unwrap(),
        ],
        parent.path(),
    );
    assert!(!u7.status.success(), "U7 corrupt bundle");
    assert!(
        !u7_dest.join(".libra").exists(),
        "U7 must not leave a target"
    );

    let url = run_libra_command(&["config", "get", "remote.origin.url"], &u1_dest);
    assert_cli_success(&url, "U8 remote.origin.url");
    let origin = String::from_utf8_lossy(&url.stdout);
    assert!(
        origin.contains("b1.bundle"),
        "U8 url must be the bundle path: {origin}"
    );
    let status = run_libra_command(&["status", "--short", "--branch"], &u1_dest);
    assert_cli_success(&status, "U8 status");
    let status_text = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_text.contains("origin/"),
        "U8 status should show origin tracking: {status_text}"
    );
}

fn committed_named_repo(parent: &std::path::Path, name: &str) -> std::path::PathBuf {
    use super::{
        assert_cli_success, configure_identity_via_cli, init_repo_via_cli, run_libra_command,
    };

    let src = parent.join(name);
    fs::create_dir_all(&src).expect("named source");
    init_repo_via_cli(&src);
    configure_identity_via_cli(&src);
    fs::write(src.join("tracked.txt"), "tracked\n").expect("tracked");
    assert_cli_success(
        &run_libra_command(&["add", ".libraignore", "tracked.txt"], &src),
        "add named repo",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], &src),
        "commit named repo",
    );
    src
}

/// M-BARE A1–A5: default dest names and bare layout (issues/474 CL-11, ADR-CL-06).
#[test]
fn test_clone_bare_and_mirror_default_names_and_layout() {
    use super::{assert_cli_success, run_libra_command};

    let parent = tempfile::tempdir().unwrap();
    let src = committed_named_repo(parent.path(), "gdeep");

    assert_cli_success(
        &run_libra_command(&["clone", "--bare", src.to_str().unwrap()], parent.path()),
        "A1 clone --bare gdeep",
    );
    let a1 = parent.path().join("gdeep.git");
    assert!(a1.join("libra.db").exists(), "A1 dest is gdeep.git");
    assert!(!parent.path().join("gdeep").join("libra.db").exists());

    let src2 = committed_named_repo(parent.path(), "mirror-src");
    assert_cli_success(
        &run_libra_command(
            &["clone", "--mirror", src2.to_str().unwrap()],
            parent.path(),
        ),
        "A2 clone --mirror mirror-src",
    );
    assert!(
        parent
            .path()
            .join("mirror-src.git")
            .join("libra.db")
            .exists(),
        "A2 dest is mirror-src.git"
    );

    let nested = parent.path().join("long/path/to/bare/dst");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--bare",
                src.to_str().unwrap(),
                nested.to_str().unwrap(),
            ],
            parent.path(),
        ),
        "A3 intermediate dirs",
    );
    assert!(nested.join("libra.db").exists(), "A3 created intermediates");

    assert!(
        !a1.join("index").exists() && !a1.join(".libra").exists(),
        "A4 bare has no worktree index"
    );
    assert!(
        !a1.join("tracked.txt").exists(),
        "A4 bare has no worktree files"
    );
    let status = run_libra_command(&["status"], &a1);
    assert!(!status.status.success(), "A4 status needs a work tree");
    let status_text = format!(
        "{}{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(
        status_text.contains("work tree"),
        "A4 status should require a work tree: {status_text}"
    );

    let bundle = parent.path().join("b1.bundle");
    assert_cli_success(
        &run_libra_command(
            &["bundle", "create", bundle.to_str().unwrap(), "--all"],
            &src,
        ),
        "A5 create bundle",
    );
    assert_cli_success(
        &run_libra_command(
            &["clone", "--bare", bundle.to_str().unwrap()],
            parent.path(),
        ),
        "A5 clone --bare bundle",
    );
    assert!(
        parent.path().join("b1.git").join("libra.db").exists(),
        "A5 bare bundle dest is b1.git"
    );
    assert_cli_success(
        &run_libra_command(&["clone", bundle.to_str().unwrap()], parent.path()),
        "A5 clone bundle",
    );
    assert!(
        parent.path().join("b1").join(".libra").exists(),
        "A5 ordinary bundle dest drops .bundle"
    );
}

/// M-TEST T1: local deterministic `clone --depth N file://` integrity matrix.
#[test]
fn test_clone_depth_integrity_local_matrix() {
    use super::{assert_cli_success, create_linear_git_repo, run_libra_command};

    let (src, _oids) = create_linear_git_repo(4);
    let url = format!("file://{}", src.path().display());
    let dest_root = tempdir().expect("t1 dest root");

    for (depth, expect_count, expect_shallow) in [
        ("1", "1", true),
        ("2", "2", true),
        ("4", "4", true),
        ("10", "4", false),
    ] {
        let dest = dest_root.path().join(format!("d{depth}"));
        let out = run_libra_command(
            &[
                "clone",
                "--depth",
                depth,
                "--single-branch",
                "--no-tags",
                &url,
                dest.to_str().unwrap(),
            ],
            dest_root.path(),
        );
        assert_cli_success(&out, &format!("T1 clone --depth {depth}"));
        assert_depth_clone_integrity(&dest, expect_count, expect_shallow);
    }
}

/// M-TEST T2 / t5601:638-643 — shallow clone locally, then clone the shallow
/// result and compare shallow files; destination must fsck clean.
///
/// Upstream uses two `git clone` steps. Libra consumes the Git-produced shallow
/// source on the second step (CL-07); the first step stays on Git so the
/// intermediate matches `ssrrcc/.git/shallow`.
#[test]
fn test_t5601_shallow_clone_locally() {
    use super::{
        assert_cli_success, create_linear_git_repo, git_success, read_shallow_oids,
        run_libra_command,
    };

    let (src, oids) = create_linear_git_repo(3);
    let tip = &oids[2];
    let dest_root = tempdir().expect("t5601 dest");
    let ssrrcc = dest_root.path().join("ssrrcc");
    git_success(
        dest_root.path(),
        &[
            "clone",
            "--depth=1",
            "--no-local",
            src.path().to_str().unwrap(),
            ssrrcc.to_str().unwrap(),
        ],
    );
    let git_shallow = ssrrcc.join(".git").join("shallow");
    assert!(git_shallow.is_file(), "git must write .git/shallow");
    let git_oids: Vec<String> = fs::read_to_string(&git_shallow)
        .expect("read git shallow")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    assert_eq!(git_oids, vec![tip.clone()], "git shallow tip");

    let ddsstt = dest_root.path().join("ddsstt");
    let out = run_libra_command(
        &["clone", ssrrcc.to_str().unwrap(), ddsstt.to_str().unwrap()],
        dest_root.path(),
    );
    assert_cli_success(&out, "t5601 clone from shallow");
    assert_eq!(
        read_shallow_oids(&ddsstt),
        git_oids,
        "shallow files must match"
    );
    assert_cli_success(&run_libra_command(&["fsck"], &ddsstt), "t5601 fsck");
}

/// M-TEST T3 / t5500:142-186 — depth 1 / depth 2 counts and fsck.
#[test]
fn test_t5500_clone_shallow_depth() {
    use super::{assert_cli_success, create_linear_git_repo, run_libra_command};

    let (src, _) = create_linear_git_repo(5);
    let url = format!("file://{}", src.path().display());
    let dest_root = tempdir().expect("t5500 dest");

    let shallow0 = dest_root.path().join("shallow0");
    let out = run_libra_command(
        &[
            "clone",
            "--no-single-branch",
            "--depth",
            "1",
            &url,
            shallow0.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "t5500 depth 1");
    assert_depth_clone_integrity(&shallow0, "1", true);

    let shallow = dest_root.path().join("shallow");
    let out = run_libra_command(
        &[
            "clone",
            "--no-single-branch",
            "--depth",
            "2",
            &url,
            shallow.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "t5500 depth 2");
    assert_depth_clone_integrity(&shallow, "2", true);
}

/// M-TEST T4 / t5537:28-44 — clone from a depth-2 shallow clone keeps history
/// and fscks clean.
#[test]
fn test_t5537_clone_from_shallow_clone() {
    use super::{assert_cli_success, create_linear_git_repo, git_success, run_libra_command};

    let (src, _) = create_linear_git_repo(4);
    let dest_root = tempdir().expect("t5537 dest");
    // Match upstream: Git produces the first shallow clone (--no-local --depth=2).
    let shallow = dest_root.path().join("shallow");
    git_success(
        dest_root.path(),
        &[
            "clone",
            "--no-local",
            "--depth=2",
            &format!("file://{}", src.path().display()),
            shallow.to_str().unwrap(),
        ],
    );
    let log = git_success_log_subjects(&shallow);
    assert_eq!(log, ["c4", "c3"], "upstream shallow log subjects: {log:?}");

    let shallow2 = dest_root.path().join("shallow2");
    let out = run_libra_command(
        &[
            "clone",
            "--no-local",
            shallow.to_str().unwrap(),
            shallow2.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "t5537 clone from shallow");
    assert_cli_success(&run_libra_command(&["fsck"], &shallow2), "t5537 fsck");
    let log2 = run_libra_command(&["log", "--format=%s"], &shallow2);
    assert_cli_success(&log2, "t5537 log");
    let log2_text = String::from_utf8_lossy(&log2.stdout);
    let subjects: Vec<&str> = log2_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(subjects, ["c4", "c3"], "t5537 log subjects: {subjects:?}");
}

fn git_success_log_subjects(repo: &std::path::Path) -> Vec<String> {
    use super::git_output;
    let out = git_output(repo, &["log", "--format=%s"]);
    assert!(
        out.status.success(),
        "git log failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// M-TEST T6 / P1b: one-commit + tag source depth clone must not report
/// `upstream is gone` on `status --short --branch`.
#[test]
fn test_clone_depth_status_not_gone_with_tag() {
    use super::{assert_cli_success, git_success, run_libra_command};

    let src = tempdir().expect("p1b source");
    git_success(src.path(), &["init", "-b", "main"]);
    git_success(src.path(), &["config", "user.name", "P1b"]);
    git_success(src.path(), &["config", "user.email", "p1b@test"]);
    git_success(src.path(), &["config", "commit.gpgsign", "false"]);
    git_success(src.path(), &["config", "tag.gpgsign", "false"]);
    fs::write(src.path().join("only.txt"), "one\n").expect("write");
    git_success(src.path(), &["add", "only.txt"]);
    git_success(src.path(), &["commit", "-m", "only"]);
    git_success(src.path(), &["tag", "-a", "v0", "-m", "v0"]);

    let dest_root = tempdir().expect("p1b dest root");
    let dest = dest_root.path().join("clone");
    let out = run_libra_command(
        &[
            "clone",
            "--depth",
            "1",
            &format!("file://{}", src.path().display()),
            dest.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "P1b clone --depth 1");
    assert_depth_clone_integrity(&dest, "1", true);

    let status = run_libra_command(&["status", "--short", "--branch"], &dest);
    assert_cli_success(&status, "P1b status");
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(
        text.lines().any(|l| l == "## main...origin/main"),
        "P1b must report healthy upstream: {text}"
    );
    assert!(
        !text.contains("[gone]") && !text.contains("upstream is gone"),
        "P1b must not report gone: {text}"
    );
}
