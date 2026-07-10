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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
}

#[tokio::test]
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
/// before fetch because that source cannot advertise shallow boundaries yet; the
/// pure post-fetch `clone_should_reject_shallow_only_for_unrequested_shallowness`
/// unit test covers the remaining remote-shallow decision.
#[test]
#[serial]
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

    // Local Libra sources cannot produce shallow boundary metadata yet, so
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
#[serial]
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
#[serial]
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
    // Libra deliberately records no `+refs/*:refs/*` fetch refspec (it would be
    // inert — Libra's fetch does not honor it).
    assert!(
        !config.contains("+refs/*:refs/*"),
        "no inert mirror fetch refspec is recorded: {config}"
    );
    // --mirror implies --bare: no working tree checked out.
    assert!(
        !mirror.join("f.txt").exists(),
        "mirror is bare (no working-tree checkout)"
    );
}

/// `--filter`/`--shallow-since`/`--shallow-exclude` are accepted but ignored
/// (Libra has no partial-clone/promisor support and its fetch only does `--depth`
/// shallow), so a COMPLETE clone is performed and each given flag emits a warning.
#[test]
#[serial]
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
#[serial]
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
