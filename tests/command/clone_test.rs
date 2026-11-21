//! Tests clone command setup to ensure objects, refs, and working copies are created correctly.
//!
//! All tests in this file are **L2 (network)**: they require
//! `LIBRA_TEST_GITHUB_LIVE=1`, `LIBRA_TEST_GITHUB_TOKEN`, and
//! `LIBRA_TEST_GITHUB_NAMESPACE` to create and push to a temporary GitHub
//! repository. Without the explicit live-test flag and credentials, the tests
//! are skipped so normal acceptance runs do not depend on external GitHub state.

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

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

#[cfg(unix)]
#[test]
#[serial(cwd, env)]
fn test_clone_ssh_publickey_auth_diagnostic() {
    use super::{
        SSH_PUBLICKEY_AUTH_OUTPUT_MODES, assert_ssh_publickey_auth_failure,
        create_ssh_publickey_auth_failure_script,
    };

    let root = tempdir().expect("failed to create SSH auth fixture root");
    let ssh = create_ssh_publickey_auth_failure_script(root.path());
    for mode in SSH_PUBLICKEY_AUTH_OUTPUT_MODES {
        let destination = root.path().join(format!("clone-{mode}"));
        let destination_arg = destination.to_string_lossy().to_string();
        assert_ssh_publickey_auth_failure(
            &["clone", "git@fixture.invalid:repo", &destination_arg],
            root.path(),
            &ssh,
            mode,
        );
        assert!(
            !destination.exists(),
            "failed SSH authentication must clean the clone destination in {mode} mode"
        );
    }
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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
        no_reject_shallow: false,
        separate_git_dir: None,
        revision: None,
        config: Vec::new(),
        no_hardlinks: false,
        reference: vec![],
        reference_if_able: vec![],
        shared: false,
        no_shared: false,
        dissociate: false,
        mirror: false,
        filter: Vec::new(),
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

fn run_libra_with_stderr_file(
    args: &[&str],
    cwd: &Path,
    stderr_path: &Path,
) -> std::process::Output {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    let global_db = home.join(".libra").join("config.db");
    let system_db = home.join(".libra").join("system-config.db");
    fs::create_dir_all(&config_home).expect("create isolated config directory");
    let stderr = fs::File::create(stderr_path).expect("create stderr capture file");

    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
        .env("LIBRA_CONFIG_SYSTEM_DB", &system_db)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(libra::utils::pager::LIBRA_TEST_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    if let Some(llvm_profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", llvm_profile_file);
    }
    command
        .output()
        .expect("failed to execute libra binary with stderr file")
}

#[test]
fn test_clone_boolean_progress_does_not_consume_url() {
    use super::{assert_cli_success, run_libra_command};

    let parent = tempdir().unwrap();
    let src = committed_named_repo(parent.path(), "progress-src");
    let src_head = run_libra_command(&["rev-parse", "HEAD"], &src);
    assert_cli_success(&src_head, "source rev-parse HEAD");
    let expected = String::from_utf8_lossy(&src_head.stdout).trim().to_string();
    let url = format!("file://{}", src.display());

    // (case name, extra clone flags, whether a `%` line is expected)
    let cases: &[(&str, &[&str], bool)] = &[
        ("progress", &["--progress"], true),
        ("no-progress", &["--no-progress"], false),
        ("progress-none", &["--progress=none"], false),
        ("progress-text", &["--progress=text"], true),
        (
            "progress-text-no-progress",
            &["--progress=text", "--no-progress"],
            false,
        ),
        ("progress-auto", &["--progress=auto"], false),
        ("quiet", &["--quiet", "--progress"], false),
        ("json", &["--json", "--progress"], false),
        ("machine", &["--machine", "--progress"], false),
        // Existing mode-value spelling must remain accepted for compatibility.
        ("progress-legacy-text", &["--progress", "text"], true),
    ];

    for &(name, extra, expect_percent) in cases {
        let destination = parent.path().join(format!("{name}-dest"));
        let stderr_path = parent.path().join(format!("{name}.stderr"));
        let mut args = vec!["clone"];
        args.extend_from_slice(extra);
        args.push(url.as_str());
        args.push(destination.to_str().expect("dest path"));
        let out = run_libra_with_stderr_file(&args, parent.path(), &stderr_path);
        assert_cli_success(&out, name);
        let stderr = fs::read_to_string(&stderr_path).expect("read captured stderr");
        assert_eq!(
            stderr.contains('%'),
            expect_percent,
            "{name}: unexpected progress output on redirected stderr: {stderr}"
        );
    }

    let global_progress_dest = parent.path().join("progress-global-dest");
    let global_progress_stderr = parent.path().join("progress-global.stderr");
    let global_progress = run_libra_with_stderr_file(
        &[
            "--color=never",
            "--progress",
            "clone",
            url.as_str(),
            global_progress_dest.to_str().expect("global dest path"),
        ],
        parent.path(),
        &global_progress_stderr,
    );
    assert_cli_success(&global_progress, "global flags before --progress clone");
    let global_stderr =
        fs::read_to_string(&global_progress_stderr).expect("read global progress stderr");
    assert!(
        global_stderr.contains('%'),
        "a global flag before --progress clone must still preserve boolean progress: {global_stderr}"
    );

    let json_progress_dest = parent.path().join("progress-json-dest");
    let json_progress_stderr = parent.path().join("progress-json.stderr");
    let json_progress = run_libra_with_stderr_file(
        &[
            "clone",
            "--progress=json",
            url.as_str(),
            json_progress_dest.to_str().expect("json dest path"),
        ],
        parent.path(),
        &json_progress_stderr,
    );
    assert_cli_success(&json_progress, "clone --progress=json");
    let json_stderr = fs::read_to_string(&json_progress_stderr).expect("read json progress stderr");
    let json_lines: Vec<&str> = json_stderr
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .collect();
    assert!(
        !json_lines.is_empty(),
        "--progress=json must emit NDJSON progress events: {json_stderr}"
    );
    for line in &json_lines {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|error| panic!("invalid NDJSON progress line {line:?}: {error}"));
    }
    assert!(
        json_stderr.contains("\"event\":\"progress\""),
        "--progress=json must emit a progress event: {json_stderr}"
    );
    assert!(
        json_stderr.contains("\"event\":\"progress_done\""),
        "--progress=json must emit a completion event: {json_stderr}"
    );

    let json_no_progress_dest = parent.path().join("progress-json-none-dest");
    let json_no_progress_stderr = parent.path().join("progress-json-none.stderr");
    let json_no_progress = run_libra_with_stderr_file(
        &[
            "clone",
            "--progress=json",
            "--no-progress",
            url.as_str(),
            json_no_progress_dest.to_str().expect("json none dest path"),
        ],
        parent.path(),
        &json_no_progress_stderr,
    );
    assert_cli_success(&json_no_progress, "clone --progress=json --no-progress");
    let json_none =
        fs::read_to_string(&json_no_progress_stderr).expect("read suppressed json progress stderr");
    for line in json_none
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
    {
        let event: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("invalid NDJSON {line:?}: {error}"));
        let name = event.get("event").and_then(|value| value.as_str());
        assert!(
            !matches!(name, Some("progress" | "progress_done")),
            "--no-progress must suppress all NDJSON progress events: {line}"
        );
    }

    let cloned_head =
        run_libra_command(&["rev-parse", "HEAD"], &parent.path().join("progress-dest"));
    assert_cli_success(&cloned_head, "cloned rev-parse HEAD");
    assert_eq!(
        String::from_utf8_lossy(&cloned_head.stdout).trim(),
        expected,
        "clone --progress must not consume the URL as the mode value"
    );
}

#[test]
fn test_clone_help_documents_boolean_progress() {
    use super::run_libra_command;

    let temp = tempdir().unwrap();
    let output = run_libra_command(&["clone", "--help"], temp.path());
    assert!(
        output.status.success(),
        "clone --help should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("--progress=<MODE>"),
        "clone help must document the explicit mode spelling: {help}"
    );
    assert!(
        help.contains("libra clone --progress"),
        "clone help must include the boolean --progress example: {help}"
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
fn test_clone_short_n_skips_checkout() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let src_head = run_libra_command(&["rev-parse", "HEAD"], src.path());
    assert_cli_success(&src_head, "source rev-parse HEAD");
    let expected = String::from_utf8_lossy(&src_head.stdout).trim().to_string();

    let work = tempdir().unwrap();
    let dst = work.path().join("short-n-dst");
    let url = format!("file://{}", src.path().display());
    let out = run_libra_command(
        &["clone", "-n", url.as_str(), dst.to_str().expect("dst path")],
        work.path(),
    );
    assert_cli_success(&out, "clone -n");

    // AC2: nothing but repository metadata was materialized. Checked before any
    // later command runs inside the destination, because the shared test
    // harness creates its fake `HOME` as `.libra-test-home` in the child cwd.
    let mut entries: Vec<String> = std::fs::read_dir(&dst)
        .expect("read clone destination")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec![".libra".to_string()],
        "clone -n must leave only `.libra` in the destination"
    );

    // AC1: the short flag still records the source HEAD.
    let cloned = run_libra_command(&["rev-parse", "HEAD"], &dst);
    assert_cli_success(&cloned, "cloned rev-parse HEAD");
    assert_eq!(
        String::from_utf8_lossy(&cloned.stdout).trim(),
        expected,
        "clone -n must set HEAD to the source commit"
    );

    // AC3: exactly one merged output line mentions `Clon`.
    let merged = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let clon_lines = merged.lines().filter(|line| line.contains("Clon")).count();
    assert_eq!(
        clon_lines, 1,
        "clone -n should print exactly one line mentioning Clon: {merged}"
    );

    // AC4: the help text advertises the short flag.
    let help = run_libra_command(&["clone", "--help"], work.path());
    assert_cli_success(&help, "clone --help");
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    assert!(
        help_stdout.contains("-n, --no-checkout"),
        "clone help must document `-n, --no-checkout`: {help_stdout}"
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

/// This test covers the accepted `--local` / `--no-local` / `-l` spellings
/// when the source is a local Libra repository. Git-source transport semantics
/// are covered by `test_clone_local_path_ignores_shallow_options_matrix`.
#[test]
fn test_clone_local_flag_accepted_for_local_source() {
    use git_internal::{hash::HashKind, internal::index::Index};

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
        let index = Index::load_with_hash_kind(HashKind::Sha1, dest.join(".libra/index"))
            .expect("load cloned index");
        let entry = index.get("f.txt", 0).expect("cloned file index entry");
        let metadata = fs::symlink_metadata(dest.join("f.txt")).expect("cloned file stat");
        assert_eq!(u64::from(entry.size), metadata.len(), "{flag}: index size");
        assert_eq!(
            entry.ctime.to_string() == "0:0",
            !cfg!(unix),
            "{flag}: index ctime"
        );
        assert_eq!(
            entry.mtime.to_string() == "0:0",
            !cfg!(unix),
            "{flag}: index mtime"
        );
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
fn test_clone_no_reject_shallow_overrides_reject_shallow() {
    use super::{assert_cli_success, create_linear_git_repo, git_success, run_libra_command};

    let (linear, _oids) = create_linear_git_repo(2);
    let root = tempdir().expect("no-reject-shallow destination root");
    let shallow = root.path().join("shallow");
    git_success(
        root.path(),
        &[
            "clone",
            "--depth",
            "1",
            "--single-branch",
            "--no-tags",
            &format!("file://{}", linear.path().display()),
            shallow.to_str().unwrap(),
        ],
    );
    assert!(
        shallow.join(".git").join("shallow").is_file(),
        "git must produce a shallow source"
    );

    // AC1: the later `--no-reject-shallow` wins, so the shallow source clones.
    let d1 = root.path().join("d1");
    let out = run_libra_command(
        &[
            "clone",
            "--reject-shallow",
            "--no-reject-shallow",
            shallow.to_str().unwrap(),
            d1.to_str().unwrap(),
        ],
        root.path(),
    );
    assert_cli_success(&out, "clone --reject-shallow --no-reject-shallow");

    // AC2: the later `--reject-shallow` wins and the shallow source is refused.
    let d2 = root.path().join("d2");
    let out = run_libra_command(
        &[
            "clone",
            "--no-reject-shallow",
            "--reject-shallow",
            shallow.to_str().unwrap(),
            d2.to_str().unwrap(),
        ],
        root.path(),
    );
    assert!(
        !out.status.success(),
        "the later --reject-shallow must be honoured"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-REPO-003"),
        "expected the LBR-REPO-003 stable code: {stderr}"
    );
    assert!(
        !d2.join(".libra").exists(),
        "a refused clone must not leave a destination"
    );
}

#[test]
fn test_clone_separate_git_dir_and_bare_mutex() {
    use sha2::{Digest, Sha256};

    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    fn blob_hashes(dir: &std::path::Path, paths: &[String]) -> Vec<(String, String)> {
        paths
            .iter()
            .map(|path| {
                let out = run_libra_command(&["cat-file", "-p", &format!("HEAD:{path}")], dir);
                assert_cli_success(&out, "cat-file HEAD path");
                (path.clone(), format!("{:x}", Sha256::digest(&out.stdout)))
            })
            .collect()
    }

    fn worktree_hashes(root: &std::path::Path) -> Vec<(String, String)> {
        fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("read worktree dir") {
                let entry = entry.expect("worktree entry");
                let name = entry.file_name();
                if dir == base && name == ".libra" {
                    continue;
                }
                let path = entry.path();
                let file_type = entry.file_type().expect("worktree entry type");
                if file_type.is_dir() {
                    walk(base, &path, out);
                } else {
                    let relative = path
                        .strip_prefix(base)
                        .expect("relative worktree path")
                        .to_string_lossy()
                        .into_owned();
                    let bytes = std::fs::read(&path).expect("read worktree file");
                    out.push((relative, format!("{:x}", Sha256::digest(&bytes))));
                }
            }
        }
        let mut files = Vec::new();
        walk(root, root, &mut files);
        files.sort();
        files
    }

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let url = format!("file://{}", src.path().display());

    // AC1/AC2/AC3: metadata in <G>, pointer at <W>/.libra, worktree checked out.
    let git_dir = root.path().join("meta");
    let work = root.path().join("w1");
    let out = run_libra_command(
        &[
            "clone",
            "--separate-git-dir",
            git_dir.to_str().expect("git dir path"),
            url.as_str(),
            work.to_str().expect("workdir path"),
        ],
        root.path(),
    );
    assert_cli_success(&out, "clone --separate-git-dir");
    assert!(
        git_dir.join("libra.db").is_file(),
        "AC1: the metadata database lives in <G>"
    );
    let pointer = std::fs::read_to_string(work.join(".libra")).expect("read the pointer file");
    let target = pointer
        .trim()
        .strip_prefix("gitdir:")
        .expect("pointer must start with `gitdir:`")
        .trim();
    assert_eq!(
        std::fs::canonicalize(target).expect("canonicalize pointer target"),
        std::fs::canonicalize(&git_dir).expect("canonicalize <G>"),
        "AC2: the pointer names <G>"
    );
    let names_out = run_libra_command(&["ls-tree", "-r", "--name-only", "HEAD"], src.path());
    assert_cli_success(&names_out, "source ls-tree");
    let names: Vec<String> = String::from_utf8_lossy(&names_out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    let mut expected = blob_hashes(src.path(), &names);
    expected.sort();
    assert_eq!(
        worktree_hashes(&work),
        expected,
        "AC3: the initial checkout matches the source HEAD tree"
    );

    // AC4/AC5: `--bare` conflicts with `--separate-git-dir`.
    let bare_git = root.path().join("g2");
    let bare_work = root.path().join("w2");
    let out = run_libra_command(
        &[
            "clone",
            "--bare",
            "--separate-git-dir",
            bare_git.to_str().expect("git dir path"),
            url.as_str(),
            bare_work.to_str().expect("workdir path"),
        ],
        root.path(),
    );
    assert!(
        !out.status.success(),
        "--bare with --separate-git-dir must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-CLI-002"),
        "AC4: expected the LBR-CLI-002 stable code: {stderr}"
    );
    assert!(
        stderr.contains("options '--bare' and '--separate-git-dir' cannot be used together"),
        "AC5: expected Git's message: {stderr}"
    );
    assert!(
        !bare_git.exists() && !bare_work.exists(),
        "a refused clone must not create either directory"
    );

    // AC6/AC7: `--mirror` implies bare, so the same mutex applies.
    let mirror_git = root.path().join("g3");
    let mirror_work = root.path().join("w3");
    let out = run_libra_command(
        &[
            "clone",
            "--mirror",
            "--separate-git-dir",
            mirror_git.to_str().expect("git dir path"),
            url.as_str(),
            mirror_work.to_str().expect("workdir path"),
        ],
        root.path(),
    );
    assert!(
        !out.status.success(),
        "--mirror with --separate-git-dir must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-CLI-002"),
        "AC6: expected the LBR-CLI-002 stable code: {stderr}"
    );
    assert!(
        stderr.contains("options '--bare' and '--separate-git-dir' cannot be used together"),
        "AC7: expected Git's message: {stderr}"
    );

    // AC8: a missing source is rejected before anything is created.
    let missing_git = root.path().join("g4");
    let missing_work = root.path().join("w4");
    let out = run_libra_command(
        &[
            "clone",
            "--separate-git-dir",
            missing_git.to_str().expect("git dir path"),
            root.path()
                .join("does-not-exist")
                .to_str()
                .expect("source path"),
            missing_work.to_str().expect("workdir path"),
        ],
        root.path(),
    );
    assert!(!out.status.success(), "a missing source must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-REPO-001"),
        "AC8: expected the LBR-REPO-001 stable code"
    );
    assert!(
        !missing_git.exists() && !missing_work.exists(),
        "a refused clone must not create either directory"
    );
}

#[test]
fn test_clone_separate_git_dir_pointer_commands() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let src_head = run_libra_command(&["rev-parse", "HEAD"], src.path());
    assert_cli_success(&src_head, "source rev-parse HEAD");
    let expected = String::from_utf8_lossy(&src_head.stdout).trim().to_string();

    let root = tempdir().unwrap();
    let git_dir = root.path().join("meta");
    let work = root.path().join("w1");
    let out = run_libra_command(
        &[
            "clone",
            "--separate-git-dir",
            git_dir.to_str().expect("git dir path"),
            &format!("file://{}", src.path().display()),
            work.to_str().expect("workdir path"),
        ],
        root.path(),
    );
    assert_cli_success(&out, "clone --separate-git-dir");

    // AC1: repository discovery follows the `gitdir:` pointer.
    let status = run_libra_command(&["status"], &work);
    assert_cli_success(&status, "AC1: status in a pointer worktree");

    // AC2: log walks the fetched history.
    let log = run_libra_command(&["log", "-n", "1"], &work);
    assert_cli_success(&log, "AC2: log -n 1 in a pointer worktree");
    assert!(
        String::from_utf8_lossy(&log.stdout).contains(&expected),
        "AC2: log must name the source HEAD commit"
    );

    // AC3: object reads resolve through the relocated metadata.
    let cat = run_libra_command(&["cat-file", "-t", "HEAD"], &work);
    assert_cli_success(&cat, "AC3: cat-file -t HEAD");
    assert_eq!(String::from_utf8_lossy(&cat.stdout), "commit\n");

    // AC4: the no-argument branch listing shows the checked-out branch.
    let branch = run_libra_command(&["branch"], &work);
    assert_cli_success(&branch, "AC4: branch in a pointer worktree");
    assert!(
        String::from_utf8_lossy(&branch.stdout).contains("main"),
        "AC4: branch must list main: {}",
        String::from_utf8_lossy(&branch.stdout)
    );

    // AC5: `alternates add <W>` records `<G>/objects`, not `<W>/.libra/objects`.
    let other = root.path().join("other");
    assert_cli_success(
        &run_libra_command(
            &["init", other.to_str().expect("other repo path")],
            root.path(),
        ),
        "init the alternates borrower",
    );
    let added = run_libra_command(
        &["alternates", "add", work.to_str().expect("workdir path")],
        &other,
    );
    assert_cli_success(&added, "AC5: alternates add");
    let alternates = std::fs::read_to_string(other.join(".libra/objects/info/alternates"))
        .expect("read the alternates file");
    let recorded: Vec<&str> = alternates
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        recorded.len(),
        1,
        "exactly one alternate must be recorded: {alternates}"
    );
    assert_eq!(
        std::fs::canonicalize(recorded[0]).expect("canonicalize the alternate"),
        std::fs::canonicalize(git_dir.join("objects")).expect("canonicalize <G>/objects"),
        "AC5: the alternate must point at <G>/objects"
    );
}

#[test]
fn test_clone_separate_git_dir_default_deny() {
    use sha2::{Digest, Sha256};

    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let git_dir = root.path().join("meta");
    let work = root.path().join("w1");
    let out = run_libra_command(
        &[
            "clone",
            "--separate-git-dir",
            git_dir.to_str().expect("git dir path"),
            &format!("file://{}", src.path().display()),
            work.to_str().expect("workdir path"),
        ],
        root.path(),
    );
    assert_cli_success(&out, "clone --separate-git-dir");

    // Snapshot of the repository state the refused commands must not change:
    // the index bytes, every reference (read from the metadata directory, where
    // discovery treats `<G>` as the storage root) and the worktree files.
    let snapshot = |work: &std::path::Path, git_dir: &std::path::Path| {
        let index = std::fs::read(git_dir.join("index")).unwrap_or_default();
        let refs = run_libra_command(&["show-ref"], git_dir);
        assert_cli_success(&refs, "snapshot show-ref in the metadata directory");
        let mut files: Vec<(String, String)> = Vec::new();
        for entry in std::fs::read_dir(work).expect("read worktree") {
            let entry = entry.expect("worktree entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".libra" || name == ".libra-test-home" {
                continue;
            }
            let path = entry.path();
            if path.is_file() {
                let bytes = std::fs::read(&path).expect("read worktree file");
                files.push((name, format!("{:x}", Sha256::digest(&bytes))));
            }
        }
        files.sort();
        (
            format!("{:x}", Sha256::digest(&index)),
            String::from_utf8_lossy(&refs.stdout).into_owned(),
            files,
        )
    };

    let before = snapshot(&work, &git_dir);

    // AC1: `restore .` is refused (it reads and rewrites the index).
    std::fs::write(work.join("tracked.txt"), "dirty\n").expect("modify the tracked file");
    let out = run_libra_command(&["restore", "."], &work);
    assert!(!out.status.success(), "AC1: restore must be refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-UNSUPPORTED-001"),
        "AC1: expected the LBR-UNSUPPORTED-001 stable code: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(work.join("tracked.txt"), "tracked\n").expect("restore the tracked file");

    // AC2: `commit` is refused.
    let out = run_libra_command(&["commit", "--allow-empty", "-m", "x"], &work);
    assert!(!out.status.success(), "AC2: commit must be refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-UNSUPPORTED-001"),
        "AC2: expected the LBR-UNSUPPORTED-001 stable code"
    );

    // AC3: creating a branch is refused.
    let out = run_libra_command(&["branch", "newbranch"], &work);
    assert!(
        !out.status.success(),
        "AC3: branch creation must be refused"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-UNSUPPORTED-001"),
        "AC3: expected the LBR-UNSUPPORTED-001 stable code"
    );

    // AC4: `checkout` is refused.
    let out = run_libra_command(&["checkout", "other"], &work);
    assert!(!out.status.success(), "AC4: checkout must be refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-UNSUPPORTED-001"),
        "AC4: expected the LBR-UNSUPPORTED-001 stable code"
    );

    let after = snapshot(&work, &git_dir);
    assert_eq!(
        before, after,
        "a refused command must not change the index, the refs or the worktree"
    );

    // The allowed read-only commands still work in the same layout.
    assert_cli_success(
        &run_libra_command(&["status"], &work),
        "status stays allowed",
    );
    assert_cli_success(
        &run_libra_command(&["branch"], &work),
        "the bare branch listing stays allowed",
    );
}

#[test]
fn test_clone_revision_single_rev() {
    use crate::command::{assert_cli_success, run_libra_command, run_libra_command_with_env};

    let root = tempdir().unwrap();
    let src = root.path().join("src");
    assert_cli_success(
        &run_libra_command(&["init", src.to_str().unwrap()], root.path()),
        "init source",
    );
    let identity: &[(&str, &str)] = &[
        ("LIBRA_COMMITTER_NAME", "t"),
        ("LIBRA_COMMITTER_EMAIL", "t@t"),
        ("GIT_AUTHOR_NAME", "t"),
        ("GIT_AUTHOR_EMAIL", "t@t"),
    ];
    let commit = |msg: &str| {
        assert_cli_success(
            &run_libra_command_with_env(&["commit", "--no-gpg-sign", "-m", msg], &src, identity),
            "commit",
        );
    };
    std::fs::write(src.join("a.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &src), "add s1");
    commit("s1");
    let s1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &src).stdout)
        .trim()
        .to_string();
    assert_cli_success(&run_libra_command(&["tag", "v0"], &src), "tag v0");
    std::fs::write(src.join("a.txt"), "2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &src), "add s2");
    commit("s2");
    let s2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &src).stdout)
        .trim()
        .to_string();
    // C2 on `side` is reachable only from that branch.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "side"], &src),
        "switch side",
    );
    std::fs::write(src.join("b.txt"), "side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], &src), "add c2");
    commit("c2");
    assert_cli_success(
        &run_libra_command(&["switch", "main"], &src),
        "back to main",
    );
    let url = format!("file://{}", src.display());

    // AC1/AC2/AC3/AC4: a bare revision clone of refs/heads/main.
    let bare = root.path().join("b");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--revision=refs/heads/main",
                "--bare",
                url.as_str(),
                bare.to_str().unwrap(),
            ],
            root.path(),
        ),
        "AC1: bare revision clone",
    );
    let head = run_libra_command(&["cat-file", "-p", "HEAD"], &bare);
    assert_cli_success(&head, "AC1: read the bare HEAD commit");
    let head_text = String::from_utf8_lossy(&head.stdout);
    assert!(head_text.contains("tree"), "AC1: HEAD is a commit");
    // HEAD resolves to S2, not to the side branch tip.
    let resolved = run_libra_command(&["rev-parse", "HEAD"], &bare);
    assert_cli_success(&resolved, "AC1: rev-parse HEAD");
    assert_eq!(
        String::from_utf8_lossy(&resolved.stdout).trim(),
        s2,
        "AC1: detached at S2"
    );

    // AC2: every object of the source is present iff reachable from S2.
    let all = run_libra_command(&["rev-list", "--objects", "--all"], &src);
    assert_cli_success(&all, "AC2: source object list");
    let reachable = run_libra_command(&["rev-list", "--objects", &s2], &src);
    assert_cli_success(&reachable, "AC2: S2 closure");
    let reachable_set: std::collections::HashSet<String> =
        String::from_utf8_lossy(&reachable.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
            .collect();
    for line in String::from_utf8_lossy(&all.stdout).lines() {
        let Some(oid) = line.split_whitespace().next() else {
            continue;
        };
        let exists = run_libra_command(&["cat-file", "-e", oid], &bare)
            .status
            .success();
        assert_eq!(
            exists,
            reachable_set.contains(oid),
            "AC2: object {oid} presence must match the requested closure"
        );
    }

    // AC3: no branch or tag rows survive (only built-in local rows).
    let branches = run_libra_command(&["branch", "--all"], &bare);
    let branches_out = String::from_utf8_lossy(&branches.stdout);
    assert!(
        !branches_out.contains("main") && !branches_out.contains("side"),
        "AC3: a revision clone keeps no branches: {branches_out}"
    );

    // AC4: no remote.origin.fetch configuration.
    let config = run_libra_command(&["config", "list"], &bare);
    let config_out = String::from_utf8_lossy(&config.stdout);
    assert!(
        !config_out.contains("remote.origin.fetch"),
        "AC4: no remote fetch refspec: {config_out}"
    );

    // AC5/AC6: a tag revision checks out S1 as a detached worktree.
    let work = root.path().join("d");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--revision=refs/tags/v0",
                url.as_str(),
                work.to_str().unwrap(),
            ],
            root.path(),
        ),
        "AC5: tag revision clone",
    );
    let status = run_libra_command(&["status", "--porcelain=v2", "--branch"], &work);
    assert_cli_success(&status, "AC5: status");
    let first_two: Vec<String> = String::from_utf8_lossy(&status.stdout)
        .lines()
        .take(2)
        .map(str::to_owned)
        .collect();
    assert_eq!(
        first_two,
        vec![
            "# branch.head (detached)".to_string(),
            format!("# branch.oid {s1}")
        ]
    );

    // AC6: only the S1 closure is present.
    let reachable_s1 = run_libra_command(&["rev-list", "--objects", &s1], &src);
    assert_cli_success(&reachable_s1, "AC6: S1 closure");
    let s1_set: std::collections::HashSet<String> = String::from_utf8_lossy(&reachable_s1.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect();
    for line in String::from_utf8_lossy(&all.stdout).lines() {
        let Some(oid) = line.split_whitespace().next() else {
            continue;
        };
        let exists = run_libra_command(&["cat-file", "-e", oid], &work)
            .status
            .success();
        assert_eq!(
            exists,
            s1_set.contains(oid),
            "AC6: object {oid} presence must match the S1 closure"
        );
    }
}

#[test]
fn test_clone_revision_conflicts() {
    use crate::command::{create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let url = format!("file://{}", src.path().display());

    for extra in [vec!["--branch", "main"], vec!["--mirror"]] {
        let mut argv = vec!["clone", "--revision=refs/tags/v1"];
        argv.extend(extra.iter().copied());
        argv.push(url.as_str());
        let dest = root.path().join("out");
        argv.push(dest.to_str().unwrap());
        let out = run_libra_command(&argv, root.path());
        assert!(!out.status.success(), "--revision with {extra:?} must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002")
                || String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-003"),
            "expected a CLI usage code: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn test_clone_config_overrides() {
    use crate::command::{
        assert_cli_success, create_committed_repo_via_cli, run_libra_command,
        run_libra_command_with_env,
    };

    fn remotes(dir: &std::path::Path) -> Vec<String> {
        let out = run_libra_command(
            &["for-each-ref", "--format=%(refname)", "refs/remotes"],
            dir,
        );
        assert_cli_success(&out, "for-each-ref refs/remotes");
        let mut set: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.trim().strip_prefix("refs/remotes/"))
            .filter_map(|rest| rest.split('/').next())
            .map(str::to_string)
            .collect();
        set.sort();
        set.dedup();
        set
    }

    let src = create_committed_repo_via_cli();
    let url = format!("file://{}", src.path().display());
    let root = tempdir().unwrap();

    // AC1: short and long forms both write the new repository's config.
    for (flag, dir) in [("-c", "d"), ("--config", "d1")] {
        let dest = root.path().join(dir);
        let out = run_libra_command(
            &[
                "clone",
                flag,
                "core.foo=bar",
                url.as_str(),
                dest.to_str().expect("dest"),
            ],
            root.path(),
        );
        assert_cli_success(&out, "AC1: clone with a config override");
        let got = run_libra_command(&["config", "get", "core.foo"], &dest);
        assert_cli_success(&got, "AC1: config get core.foo");
        assert_eq!(String::from_utf8_lossy(&got.stdout), "bar\n");
    }

    // AC2: repeated entries are stored in order.
    let d2 = root.path().join("d2");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "-c",
                "a.b=1",
                "-c",
                "a.b=2",
                url.as_str(),
                d2.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "AC2: two entries",
    );
    let all = run_libra_command(&["config", "get", "--all", "a.b"], &d2);
    assert_cli_success(&all, "AC2: config get --all");
    assert_eq!(String::from_utf8_lossy(&all.stdout), "1\n2\n");

    // AC3: clone.defaultRemoteName applies before the initial fetch.
    let d3 = root.path().join("d3");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "-c",
                "clone.defaultRemoteName=upstream",
                url.as_str(),
                d3.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "AC3: defaultRemoteName",
    );
    assert_eq!(remotes(&d3), vec!["upstream".to_string()]);

    // AC4: an explicit `--origin` wins over the config value.
    let d4 = root.path().join("d4");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "-c",
                "clone.defaultRemoteName=inline",
                "--origin",
                "from_option",
                url.as_str(),
                d4.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "AC4: --origin wins",
    );
    assert_eq!(remotes(&d4), vec!["from_option".to_string()]);

    // AC5: the global config provides the default remote name.
    let global = root.path().join("global.db");
    let env: &[(&str, &str)] = &[(
        "LIBRA_CONFIG_GLOBAL_DB",
        global.to_str().expect("global db"),
    )];
    assert_cli_success(
        &run_libra_command_with_env(
            &[
                "config",
                "set",
                "--global",
                "clone.defaultRemoteName",
                "from_config",
            ],
            root.path(),
            env,
        ),
        "AC5: config set --global",
    );
    let d6 = root.path().join("d6");
    let out = run_libra_command_with_env(
        &["clone", url.as_str(), d6.to_str().expect("dest")],
        root.path(),
        env,
    );
    assert_cli_success(&out, "AC5: clone with the global default");
    assert_eq!(remotes(&d6), vec!["from_config".to_string()]);

    // AC6: an empty key is refused.
    let d5 = root.path().join("d5");
    let out = run_libra_command(
        &[
            "clone",
            "-c",
            "=v",
            url.as_str(),
            d5.to_str().expect("dest"),
        ],
        root.path(),
    );
    assert!(!out.status.success(), "AC6: an empty key must be refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "AC6: expected LBR-CLI-002: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn test_clone_local_hardlinks_and_no_local() {
    use std::os::unix::fs::MetadataExt;

    use crate::command::{
        assert_cli_success, create_committed_repo_via_cli, run_libra_command,
        run_libra_command_with_env,
    };

    type ObjFile = (std::path::PathBuf, u64, u64, Vec<u8>);

    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<ObjFile>) {
        for entry in std::fs::read_dir(dir).expect("read object dir") {
            let entry = entry.expect("object entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("object type");
            if file_type.is_dir() {
                walk(base, &path, out);
                continue;
            }
            let relative = path.strip_prefix(base).expect("relative").to_path_buf();
            if relative == std::path::Path::new("info/alternates") {
                continue;
            }
            let meta = std::fs::metadata(&path).expect("metadata");
            let bytes = std::fs::read(&path).expect("read object");
            out.push((relative, meta.dev(), meta.ino(), bytes));
        }
    }

    fn object_files(root: &std::path::Path) -> Vec<ObjFile> {
        let mut files = Vec::new();
        walk(root, root, &mut files);
        files.sort();
        files
    }

    let src = create_committed_repo_via_cli();
    let src_objects = src.path().join(".libra/objects");
    let source_files = object_files(&src_objects);
    assert!(!source_files.is_empty(), "the fixture must have objects");
    let url = format!("file://{}", src.path().display());
    let root = tempdir().unwrap();

    // AC1: `--local` and the default plain local path share inodes.
    for (flags, dir) in [(vec!["--local"], "d"), (vec![], "d1")] {
        let dest = root.path().join(dir);
        let mut argv = vec!["clone"];
        argv.extend(flags.iter().copied());
        argv.push(src.path().to_str().expect("src"));
        argv.push(dest.to_str().expect("dest"));
        assert_cli_success(&run_libra_command(&argv, root.path()), "AC1: local clone");
        for (relative, dev, ino, _bytes) in &source_files {
            let target = dest.join(".libra/objects").join(relative);
            let meta = std::fs::metadata(&target)
                .unwrap_or_else(|error| panic!("AC1: {} missing: {error}", target.display()));
            assert_eq!(
                (meta.dev(), meta.ino()),
                (*dev, *ino),
                "AC1: {} must share the source inode",
                relative.display()
            );
        }
    }

    // AC3: `--no-local` copies, never shares inodes.
    let d2 = root.path().join("d2");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--no-local",
                url.as_str(),
                d2.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "AC3: --no-local clone",
    );
    assert_cli_success(
        &run_libra_command(&["fsck"], &d2),
        "AC3: destination is complete",
    );
    let source_inodes: std::collections::HashSet<(u64, u64)> = source_files
        .iter()
        .map(|(_, dev, ino, _)| (*dev, *ino))
        .collect();
    for (relative, dev, ino, _bytes) in object_files(&d2.join(".libra/objects")) {
        assert!(
            !source_inodes.contains(&(dev, ino)),
            "AC3: {} must not share the source inode",
            relative.display()
        );
    }

    // AC4: the injected first hard-link failure falls back to copying.
    let d3 = root.path().join("d3");
    let env: &[(&str, &str)] = &[
        ("LIBRA_TEST", "1"),
        ("LIBRA_TEST_CLONE_HARDLINK_FAIL", "first"),
    ];
    assert_cli_success(
        &run_libra_command_with_env(
            &[
                "clone",
                src.path().to_str().expect("src"),
                d3.to_str().expect("dest"),
            ],
            root.path(),
            env,
        ),
        "AC4: fallback clone",
    );
    assert_cli_success(
        &run_libra_command(&["fsck"], &d3),
        "AC4: destination is complete",
    );
    for (relative, dev, ino, _bytes) in object_files(&d3.join(".libra/objects")) {
        assert!(
            !source_inodes.contains(&(dev, ino)),
            "AC4: {} must be copied after the injected failure",
            relative.display()
        );
    }

    // AC5: an explicit `--local` fails hard when hard-linking fails.
    let d4 = root.path().join("d4");
    let out = run_libra_command_with_env(
        &[
            "clone",
            "--local",
            src.path().to_str().expect("src"),
            d4.to_str().expect("dest"),
        ],
        root.path(),
        env,
    );
    assert!(!out.status.success(), "AC5: explicit --local must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-IO-002"),
        "AC5: expected LBR-IO-002: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn test_clone_local_rewrites_relative_alternates() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();

    // R owns the objects.
    let r = root.path().join("R");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                src.path().to_str().expect("src"),
                r.to_str().expect("R"),
            ],
            root.path(),
        ),
        "clone R",
    );

    // SA borrows R through a RELATIVE alternates entry and owns no objects.
    let sa = root.path().join("SA");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                src.path().to_str().expect("src"),
                sa.to_str().expect("SA"),
            ],
            root.path(),
        ),
        "clone SA",
    );
    for entry in std::fs::read_dir(sa.join(".libra/objects")).expect("read SA objects") {
        let entry = entry.expect("SA object entry");
        if entry.file_name() == "info" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path).expect("clear SA objects");
        } else {
            std::fs::remove_file(&path).expect("clear SA objects");
        }
    }
    std::fs::write(
        sa.join(".libra/objects/info/alternates"),
        "../../../R/.libra/objects\n",
    )
    .expect("write the relative alternates");
    assert_cli_success(
        &run_libra_command(&["cat-file", "-p", "HEAD:tracked.txt"], &sa),
        "the relative alternates resolve inside SA",
    );

    // Cloning SA must rewrite the entry to an absolute path.
    let d8 = root.path().join("d8");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                sa.to_str().expect("SA"),
                d8.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "AC2: clone a source with relative alternates",
    );
    let alternates = std::fs::read_to_string(d8.join(".libra/objects/info/alternates"))
        .expect("read the destination alternates");
    let lines: Vec<&str> = alternates
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(
        !lines.is_empty(),
        "the destination must keep the borrowed store"
    );
    for line in lines {
        assert!(
            std::path::Path::new(line.trim()).is_absolute(),
            "AC2: alternates entries must be absolute: {line}"
        );
    }
    let cat = run_libra_command(&["cat-file", "-p", "HEAD:tracked.txt"], &d8);
    assert_cli_success(
        &cat,
        "AC2: the borrowed blob is readable through the rewrite",
    );
}

#[cfg(unix)]
#[test]
fn test_clone_no_hardlinks_copies_objects() {
    use std::os::unix::fs::MetadataExt;

    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    fn walk(
        base: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, u64, u64, Vec<u8>)>,
    ) {
        for entry in std::fs::read_dir(dir).expect("read object dir") {
            let entry = entry.expect("object entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("object type");
            if file_type.is_dir() {
                walk(base, &path, out);
                continue;
            }
            let relative = path.strip_prefix(base).expect("relative").to_path_buf();
            if relative == std::path::Path::new("info/alternates") {
                continue;
            }
            let meta = std::fs::metadata(&path).expect("metadata");
            let bytes = std::fs::read(&path).expect("read object");
            out.push((relative, meta.dev(), meta.ino(), bytes));
        }
    }

    let src = create_committed_repo_via_cli();
    let src_objects = src.path().join(".libra/objects");
    let mut source_files = Vec::new();
    walk(&src_objects, &src_objects, &mut source_files);
    source_files.sort();
    assert!(!source_files.is_empty());

    let root = tempdir().unwrap();
    let dest = root.path().join("d");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--no-hardlinks",
                src.path().to_str().expect("src"),
                dest.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "AC1: --no-hardlinks clone",
    );
    assert_cli_success(
        &run_libra_command(&["fsck"], &dest),
        "AC1: destination complete",
    );

    let mut dest_files = Vec::new();
    walk(
        &dest.join(".libra/objects"),
        &dest.join(".libra/objects"),
        &mut dest_files,
    );
    dest_files.sort();
    assert_eq!(
        dest_files.len(),
        source_files.len(),
        "AC1: every source object must be copied"
    );
    for ((src_rel, _, _, src_bytes), (dest_rel, dev, ino, dest_bytes)) in
        source_files.iter().zip(dest_files.iter())
    {
        assert_eq!(src_rel, dest_rel, "AC1: object layout matches");
        assert_eq!(src_bytes, dest_bytes, "AC1: identical content");
        assert!(
            !(*dev
                == source_files
                    .iter()
                    .find(|(r, _, _, _)| r == src_rel)
                    .map(|(_, d, i, _)| (*d, *i))
                    .unwrap()
                    .0
                && *ino
                    == source_files
                        .iter()
                        .find(|(r, _, _, _)| r == src_rel)
                        .map(|(_, d, i, _)| (*d, *i))
                        .unwrap()
                        .1),
            "AC1: {} must not share the source inode",
            dest_rel.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn test_clone_path_git_suffix_fallback() {
    use std::os::unix::fs::MetadataExt;

    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let src_head = run_libra_command(&["rev-parse", "HEAD"], src.path());
    assert_cli_success(&src_head, "source rev-parse HEAD");
    let expected = String::from_utf8_lossy(&src_head.stdout).trim().to_string();
    let bare_url = format!("file://{}", src.path().display());

    // AC1: `clone foo` finds the bare `foo.git` next to it.
    let t1 = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(
            &["clone", "--bare", bare_url.as_str(), "foo.git"],
            t1.path(),
        ),
        "create foo.git",
    );
    assert_cli_success(
        &run_libra_command(&["clone", "foo"], t1.path()),
        "AC1: clone foo resolves foo.git",
    );
    let head = run_libra_command(&["rev-parse", "HEAD"], &t1.path().join("foo"));
    assert_cli_success(&head, "AC1: cloned HEAD");
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), expected);

    // AC2/AC3: a `file://` URL falls back too, and never hard-links.
    let t2 = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(
            &["clone", "--bare", bare_url.as_str(), "foo.git"],
            t2.path(),
        ),
        "create the second foo.git",
    );
    let dst = t2.path().join("dst");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                &format!("file://{}/foo", t2.path().display()),
                dst.to_str().expect("dst"),
            ],
            t2.path(),
        ),
        "AC2: file:// falls back to foo.git",
    );
    let head = run_libra_command(&["rev-parse", "HEAD"], &dst);
    assert_cli_success(&head, "AC2: cloned HEAD");
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), expected);

    fn walk(dir: &std::path::Path, out: &mut Vec<(u64, u64)>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("type");
            if file_type.is_dir() {
                walk(&path, out);
            } else {
                let meta = std::fs::metadata(&path).expect("meta");
                out.push((meta.dev(), meta.ino()));
            }
        }
    }
    let mut source_inodes = Vec::new();
    walk(
        &t2.path().join("foo.git").join("objects"),
        &mut source_inodes,
    );
    let mut dest_inodes = Vec::new();
    walk(&dst.join(".libra/objects"), &mut dest_inodes);
    for inode in dest_inodes {
        assert!(
            !source_inodes.contains(&inode),
            "AC3: file:// must not hard-link objects"
        );
    }
}

#[test]
fn clone_reference_borrows_every_object() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let reference = root.path().join("R");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                src.path().to_str().expect("src"),
                reference.to_str().expect("reference"),
            ],
            root.path(),
        ),
        "clone the reference repository",
    );

    let dest = root.path().join("d");
    let out = run_libra_command(
        &[
            "clone",
            "--json",
            "--reference",
            reference.to_str().expect("reference"),
            &format!("file://{}", src.path().display()),
            dest.to_str().expect("dest"),
        ],
        root.path(),
    );
    assert_cli_success(&out, "clone --reference --json");

    // AC2: nothing is transferred from the source.
    let payload = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(payload.trim()).expect("clone json");
    assert_eq!(
        value["data"]["objects_fetched"]
            .as_u64()
            .unwrap_or(u64::MAX),
        0,
        "AC2: --reference must not transfer objects: {payload}"
    );

    // AC3: the destination object store itself stays empty.
    fn count_files(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| {
                        let path = entry.path();
                        if path.is_dir() {
                            if entry.file_name() == "info" {
                                0
                            } else {
                                count_files(&path)
                            }
                        } else {
                            1
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    assert_eq!(
        count_files(&dest.join(".libra/objects")),
        0,
        "AC3: the destination object store is empty"
    );

    // AC4: objects are readable through the borrowed store.
    assert_cli_success(
        &run_libra_command(&["cat-file", "-p", "HEAD:tracked.txt"], &dest),
        "AC4: cat-file through the alternate",
    );
}

#[test]
fn clone_promisor_lazy_fetches_a_missing_object() {
    use crate::command::{
        assert_cli_success, create_committed_repo_via_cli, run_libra_command,
        run_libra_command_with_env,
    };

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let clone = |dir: &str| {
        let dest = root.path().join(dir);
        assert_cli_success(
            &run_libra_command(
                &[
                    "clone",
                    "--no-local",
                    &format!("file://{}", src.path().display()),
                    dest.to_str().expect("dest"),
                ],
                root.path(),
            ),
            "clone --no-local",
        );
        assert_cli_success(
            &run_libra_command(
                &["config", "set", "extensions.partialClone", "origin"],
                &dest,
            ),
            "mark the clone as partial",
        );
        // Empty the local object store: every object is remote now.
        let packs = dest.join(".libra/objects/pack");
        if packs.is_dir() {
            for entry in std::fs::read_dir(&packs).expect("read the pack dir") {
                let path = entry.expect("pack entry").path();
                let _ = std::fs::remove_file(path);
            }
        }
        dest
    };

    // AC1/AC2/AC4: the read fetches the closure on demand, once, and persists it.
    let d = clone("d");
    let env: &[(&str, &str)] = &[("LIBRA_TEST", "1")];
    let out = run_libra_command_with_env(&["cat-file", "-p", "HEAD:tracked.txt"], &d, env);
    assert_cli_success(&out, "AC1: cat-file through the promisor remote");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lazy_lines = stderr
        .lines()
        .filter(|line| line.starts_with("libra-test: lazy-fetch"))
        .count();
    assert_eq!(lazy_lines, 1, "AC2: exactly one lazy fetch: {stderr}");
    assert!(
        run_libra_command(&["cat-file", "-p", "HEAD:tracked.txt"], &d)
            .status
            .success(),
        "AC4: the fetched objects are persisted"
    );
}

#[test]
fn clone_rejects_unimplemented_object_filters() {
    use crate::command::{create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    for (index, filter) in [
        "tree:0",
        "tree:1",
        "tree:2",
        "object:type=blob",
        "object:type=tree",
        "object:type=commit",
        "blob:limit=1024",
        "sparse:path=/tmp/sparse",
    ]
    .into_iter()
    .enumerate()
    {
        let dest = root.path().join(format!("r{index}"));
        let out = run_libra_command(
            &[
                "clone",
                "--no-local",
                "--bare",
                &format!("--filter={filter}"),
                src.path().to_str().expect("src"),
                dest.to_str().expect("dest"),
            ],
            root.path(),
        );
        assert!(!out.status.success(), "filter {filter} must be refused");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("LBR-UNSUPPORTED-001"),
            "filter {filter}: expected LBR-UNSUPPORTED-001: {stderr}"
        );
        assert!(
            !dest.exists(),
            "filter {filter} must not create the destination"
        );
    }
}

#[test]
fn clone_validates_sparse_oid_filters() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let clone_filter = |filter: &str, dir: &str| {
        let dest = root.path().join(dir);
        run_libra_command(
            &[
                "clone",
                "--no-local",
                "--bare",
                filter,
                src.path().to_str().expect("src"),
                dest.to_str().expect("dest"),
            ],
            root.path(),
        )
    };

    // AC1/AC2: a missing blob is an access error with Git's wording.
    let out = clone_filter("--filter=sparse:oid=main:no-such-name", "e1");
    assert!(!out.status.success(), "AC1: missing sparse blob must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("LBR-CLI-003"), "AC1: {stderr}");
    assert!(
        stderr.contains("unable to access sparse blob in"),
        "AC2: {stderr}"
    );

    // AC3/AC4: a commit (not a blob) is a parse error.
    let out = clone_filter("--filter=sparse:oid=main", "e2");
    assert!(!out.status.success(), "AC3: a commit oid must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("LBR-CLI-003"), "AC3: {stderr}");
    assert!(
        stderr.contains("unable to parse sparse filter data in"),
        "AC4: {stderr}"
    );

    // AC6: a well-formed rule blob is accepted.
    std::fs::write(src.path().join("only-one"), "/a.txt\n").expect("write the rule file");
    let add = run_libra_command(&["add", "only-one"], src.path());
    assert_cli_success(&add, "stage the rule file");
    let commit = run_libra_command(&["commit", "-m", "add the sparse rule file"], src.path());
    assert_cli_success(&commit, "commit the rule file");
    let out = clone_filter("--filter=sparse:oid=main:only-one", "e3");
    assert_cli_success(&out, "AC6: the rule blob is accepted");
}

#[test]
fn test_clone_filter_promisor_config_and_combine() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    std::fs::create_dir_all(src.path().join("sub")).expect("mkdir sub");
    std::fs::write(src.path().join("sub/inner.txt"), "inner\n").expect("write inner");
    assert_cli_success(
        &run_libra_command(&["add", "sub/inner.txt"], src.path()),
        "stage the nested file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add a nested directory"], src.path()),
        "commit the nested file",
    );
    let source_url = format!("file://{}", src.path().display());
    let root = tempdir().unwrap();

    let bare_clone = |name: &str, extra: &[&str]| {
        let dest = root.path().join(name);
        let mut argv = vec!["clone", "--bare"];
        argv.extend_from_slice(extra);
        argv.push(&source_url);
        let dest_str = dest.to_str().expect("dest").to_string();
        argv.push(&dest_str);
        assert_cli_success(
            &run_libra_command(&argv, root.path()),
            "bare blob:none clone",
        );
        dest
    };
    let config_get = |repo: &std::path::Path, key: &str| {
        let out = run_libra_command(&["config", "get", key], repo);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // AC1/AC2/AC3: the promisor rows follow the effective remote name.
    let d = bare_clone("d", &["-o", "upstream", "--filter=blob:none"]);
    for (key, expected) in [
        ("remote.upstream.promisor", "true"),
        ("remote.upstream.partialclonefilter", "blob:none"),
        ("extensions.partialClone", "upstream"),
    ] {
        assert_eq!(config_get(&d, key), expected, "AC1: {key}");
    }
    let d2 = bare_clone(
        "d2",
        &["-c", "clone.defaultRemoteName=cfg", "--filter=blob:none"],
    );
    for (key, expected) in [
        ("remote.cfg.promisor", "true"),
        ("remote.cfg.partialclonefilter", "blob:none"),
        ("extensions.partialClone", "cfg"),
    ] {
        assert_eq!(config_get(&d2, key), expected, "AC2: {key}");
    }
    let d3 = bare_clone("d3", &["--filter=blob:none"]);
    for (key, expected) in [
        ("remote.origin.promisor", "true"),
        ("remote.origin.partialclonefilter", "blob:none"),
        ("extensions.partialClone", "origin"),
    ] {
        assert_eq!(config_get(&d3, key), expected, "AC3: {key}");
    }

    // AC4: commits and trees present, blobs absent, in all three clones.
    let listing = run_libra_command(&["rev-list", "--objects", "--all"], src.path());
    assert_cli_success(&listing, "AC4: list the source objects");
    let mut checked = 0usize;
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let Some(oid) = line.split_whitespace().next() else {
            continue;
        };
        if oid.len() < 8 {
            continue;
        }
        let kind = run_libra_command(&["cat-file", "-t", oid], src.path());
        let kind = String::from_utf8_lossy(&kind.stdout).trim().to_string();
        if kind.is_empty() {
            continue;
        }
        let expect_present = kind != "blob";
        for repo in [&d, &d2, &d3] {
            // `--offline` keeps the promisor lazy fetch from materialising the
            // blob during the existence check (CL-29).
            let present = run_libra_command(&["--offline", "cat-file", "-e", oid], repo)
                .status
                .success();
            assert_eq!(present, expect_present, "AC4: {oid} ({kind}) in {:?}", repo);
            checked += 1;
        }
    }
    assert!(checked > 0, "AC4: the object set must not be empty");

    // AC5/AC6: combine forms and differing repeats fail closed.
    for (index, spec) in [
        "combine:blob:none+tree:2",
        "combine:tree:2+blob:none",
        "combine:blob:none+object:type=blob",
        "combine:blob:none+blob:limit=1024",
    ]
    .into_iter()
    .enumerate()
    {
        let dest = root.path().join(format!("c{index}"));
        let out = run_libra_command(
            &[
                "clone",
                &format!("--filter={spec}"),
                &source_url,
                dest.to_str().expect("dest"),
            ],
            root.path(),
        );
        assert!(!out.status.success(), "AC5: {spec} must be refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-UNSUPPORTED-001"),
            "AC5: {spec}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    for (index, pair) in [
        ("tree:1", "tree:2"),
        ("tree:2", "blob:none"),
        ("blob:none", "object:type=blob"),
        ("blob:none", "blob:limit=1024"),
    ]
    .into_iter()
    .enumerate()
    {
        let dest = root.path().join(format!("m{index}"));
        let out = run_libra_command(
            &[
                "clone",
                &format!("--filter={}", pair.0),
                &format!("--filter={}", pair.1),
                &source_url,
                dest.to_str().expect("dest"),
            ],
            root.path(),
        );
        assert!(!out.status.success(), "AC6: {pair:?} must be refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-UNSUPPORTED-001"),
            "AC6: {pair:?}"
        );
    }

    // AC7: a remote that does not advertise `filter` fails closed.
    let dest = root.path().join("e3");
    let out = crate::command::run_libra_command_with_env(
        &[
            "clone",
            "--filter=blob:none",
            &source_url,
            dest.to_str().expect("dest"),
        ],
        root.path(),
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_UPLOAD_PACK_NO_FILTER", "1"),
        ],
    );
    // CL-24 rewrote this transition assertion: a remote without the capability
    // falls back to a full clone instead of refusing.
    assert!(
        out.status.success(),
        "AC7: the fallback clone succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_clone_combine_tree_depth_and_fallback() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    std::fs::create_dir_all(src.path().join("sub")).expect("mkdir sub");
    std::fs::write(src.path().join("sub/inner.txt"), "inner\n").expect("write inner");
    assert_cli_success(
        &run_libra_command(&["add", "sub/inner.txt"], src.path()),
        "stage the nested file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add a nested directory"], src.path()),
        "commit the nested file",
    );
    let source_url = format!("file://{}", src.path().display());
    let root = tempdir().unwrap();

    // Collect the source's objects, their types, and each commit's root tree.
    let listing = run_libra_command(&["rev-list", "--objects", "--all"], src.path());
    assert_cli_success(&listing, "list the source objects");
    let mut commits = Vec::new();
    let mut blobs = Vec::new();
    let mut trees = Vec::new();
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let Some(oid) = line.split_whitespace().next() else {
            continue;
        };
        let kind = run_libra_command(&["cat-file", "-t", oid], src.path());
        match String::from_utf8_lossy(&kind.stdout).trim() {
            "commit" => commits.push(oid.to_string()),
            "blob" => blobs.push(oid.to_string()),
            "tree" => trees.push(oid.to_string()),
            _ => {}
        }
    }
    assert!(!commits.is_empty() && !blobs.is_empty(), "fixture kinds");
    let mut root_trees = Vec::new();
    for commit in &commits {
        let out = run_libra_command(&["rev-parse", &format!("{commit}^{{tree}}")], src.path());
        let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !tree.is_empty() {
            root_trees.push(tree);
        }
    }
    assert!(!root_trees.is_empty(), "root trees");
    let non_root_trees = trees
        .iter()
        .filter(|tree| !root_trees.contains(tree))
        .cloned()
        .collect::<Vec<_>>();

    let clone_bare = |name: &str, filters: &[&str]| {
        let dest = root.path().join(name);
        let mut argv = vec!["clone", "--bare"];
        for filter in filters {
            argv.push(filter);
        }
        argv.push(&source_url);
        let dest_str = dest.to_str().expect("dest").to_string();
        argv.push(&dest_str);
        assert_cli_success(&run_libra_command(&argv, root.path()), "bare combine clone");
        dest
    };
    // `--offline` keeps the promisor lazy fetch from materialising an object
    // during the existence check (CL-29).
    let exists = |repo: &std::path::Path, oid: &str| {
        run_libra_command(&["--offline", "cat-file", "-e", oid], repo)
            .status
            .success()
    };

    // AC1: the canonical filter value is recorded.
    let d = clone_bare("d", &["--filter=blob:none", "--filter=tree:1"]);
    let out = run_libra_command(&["config", "get", "remote.origin.partialclonefilter"], &d);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "combine:blob:none+tree:1",
        "AC1"
    );

    // AC2/AC3/AC4/AC5: commits and root trees stay, deeper trees and blobs go.
    for commit in &commits {
        assert!(exists(&d, commit), "AC2: commit {commit}");
    }
    for tree in &root_trees {
        assert!(exists(&d, tree), "AC3: root tree {tree}");
    }
    for tree in &non_root_trees {
        assert!(
            !exists(&d, tree),
            "AC4: non-root tree {tree} must be absent"
        );
    }
    for blob in &blobs {
        assert!(!exists(&d, blob), "AC5: blob {blob} must be absent");
    }

    // AC6: the three equivalent forms produce the same object set as `d`.
    for (index, filters) in [
        vec!["--filter=combine:blob:none+tree:1"],
        vec!["--filter=combine:tree:1+blob:none"],
        vec!["--filter=tree:1", "--filter=blob:none"],
    ]
    .into_iter()
    .enumerate()
    {
        let repo = clone_bare(&format!("e{index}"), &filters);
        for oid in commits
            .iter()
            .chain(root_trees.iter())
            .chain(non_root_trees.iter())
            .chain(blobs.iter())
        {
            assert_eq!(
                exists(&repo, oid),
                exists(&d, oid),
                "AC6: {filters:?} must match the canonical clone for {oid}"
            );
        }
    }

    // AC7: without the `filter` capability every form falls back to a full clone.
    for (index, filters) in [
        vec!["--filter=blob:none"],
        vec!["--filter=blob:none", "--filter=tree:1"],
        vec!["--filter=tree:1", "--filter=blob:none"],
        vec!["--filter=combine:blob:none+tree:1"],
        vec!["--filter=combine:tree:1+blob:none"],
    ]
    .into_iter()
    .enumerate()
    {
        let dest = root.path().join(format!("f{index}"));
        let mut argv = vec!["clone", "--bare"];
        argv.extend(filters.iter().copied());
        argv.push(&source_url);
        let dest_str = dest.to_str().expect("dest").to_string();
        argv.push(&dest_str);
        let out = crate::command::run_libra_command_with_env(
            &argv,
            root.path(),
            &[
                ("LIBRA_TEST", "1"),
                ("LIBRA_TEST_UPLOAD_PACK_NO_FILTER", "1"),
            ],
        );
        assert_cli_success(&out, "AC7: fallback clone");
        for oid in commits.iter().chain(trees.iter()).chain(blobs.iter()) {
            assert!(exists(&dest, oid), "AC7: {filters:?} must keep {oid}");
        }
    }

    // AC8: other combinations keep failing closed.
    for (index, sample) in [
        vec!["--filter=combine:blob:none+tree:2"],
        vec!["--filter=combine:blob:none+object:type=blob"],
        vec!["--filter=blob:none", "--filter=tree:2"],
        vec!["--filter=blob:none", "--filter=object:type=blob"],
    ]
    .into_iter()
    .enumerate()
    {
        let dest = root.path().join(format!("g{index}"));
        let mut argv = vec!["clone"];
        argv.extend(sample.iter().copied());
        argv.push(&source_url);
        let dest_str = dest.to_str().expect("dest").to_string();
        argv.push(&dest_str);
        let out = run_libra_command(&argv, root.path());
        assert!(!out.status.success(), "AC8: {sample:?} must be refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-UNSUPPORTED-001"),
            "AC8: {sample:?}"
        );
    }
}

#[test]
fn test_clone_sparse_oid_filter_and_missing_boundary() {
    use crate::command::{assert_cli_success, run_libra_command};

    let source = tempdir().expect("source dir");
    let sp = source.path();
    assert_cli_success(&run_libra_command(&["init"], sp), "init source");
    run_libra_command(&["config", "set", "user.name", "t"], sp);
    run_libra_command(&["config", "set", "user.email", "t@t"], sp);
    std::fs::write(sp.join("one.t"), "one\n").expect("write one.t");
    std::fs::write(sp.join("two.t"), "two\n").expect("write two.t");
    std::fs::write(sp.join("only-one"), "/one.t\n").expect("write only-one");
    assert_cli_success(&run_libra_command(&["add", "."], sp), "stage the fixture");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], sp),
        "commit the fixture",
    );

    let rev_parse = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], sp);
        assert_cli_success(&out, "rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let one_blob = rev_parse("HEAD:one.t");
    let two_blob = rev_parse("HEAD:two.t");
    let rules_blob = rev_parse("HEAD:only-one");
    assert!(
        one_blob != two_blob && one_blob != rules_blob && two_blob != rules_blob,
        "the three fixture blobs must differ"
    );

    let root = tempdir().expect("dest root");
    let dest = root.path().join("d");
    let source_url = format!("file://{}", sp.display());
    let clone = run_libra_command(
        &[
            "clone",
            "--bare",
            "--filter=sparse:oid=main:only-one",
            &source_url,
            dest.to_str().expect("dest"),
        ],
        root.path(),
    );
    // AC1
    assert_cli_success(&clone, "AC1: sparse:oid clone");

    // AC2: commits and trees stay; only the matched blob is present.
    let listing = run_libra_command(&["rev-list", "--objects", "--all"], sp);
    assert_cli_success(&listing, "AC2: list the source objects");
    let mut checked = 0usize;
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let Some(oid) = line.split_whitespace().next() else {
            continue;
        };
        if oid.len() < 8 {
            continue;
        }
        let kind = run_libra_command(&["cat-file", "-t", oid], sp);
        let kind = String::from_utf8_lossy(&kind.stdout).trim().to_string();
        if kind.is_empty() {
            continue;
        }
        let present = run_libra_command(&["--offline", "cat-file", "-e", oid], &dest)
            .status
            .success();
        let expected = kind == "commit" || kind == "tree" || oid == one_blob;
        assert_eq!(present, expected, "AC2: {oid} ({kind})");
        checked += 1;
    }
    assert!(checked > 0, "AC2: the object set must not be empty");

    // AC3/AC6: the promisor rows record the filter verbatim.
    let get = |key: &str| {
        let out = run_libra_command(&["config", "get", key], &dest);
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    assert_eq!(get("remote.origin.promisor"), "true\n", "AC3");
    assert_eq!(
        get("remote.origin.partialclonefilter"),
        "sparse:oid=main:only-one\n",
        "AC6"
    );

    // AC4: the bare clone still records the remote-tracking ref.
    let show_ref = run_libra_command(&["show-ref"], &dest);
    assert!(
        String::from_utf8_lossy(&show_ref.stdout).contains("refs/remotes/origin/main"),
        "AC4: {}",
        String::from_utf8_lossy(&show_ref.stdout)
    );

    // AC5: the filtered-out blob is fetched on demand.
    let source_content = run_libra_command(&["cat-file", "-p", "HEAD:two.t"], sp);
    assert_cli_success(&source_content, "AC5: source content");
    let fetched = run_libra_command(&["cat-file", "-p", &two_blob], &dest);
    assert_cli_success(&fetched, "AC5: on-demand blob fetch");
    assert_eq!(
        fetched.stdout, source_content.stdout,
        "AC5: fetched contents"
    );
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
        &run_libra_command(&["add", "tracked.txt"], &src),
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

#[test]
#[serial(cwd)]
fn blake3_clone_round_trip() {
    use crate::command::{
        assert_cli_success, create_committed_repo_with_format, run_libra_command,
    };

    let source = create_committed_repo_with_format("blake3");
    let dest_root = tempdir().expect("clone dest root");
    let dest = dest_root.path().join("blake3-clone");
    let out = run_libra_command(
        &[
            "clone",
            source.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "blake3 clone round-trip");
    let format = run_libra_command(&["config", "--get", "core.objectformat"], &dest);
    assert_cli_success(&format, "read cloned objectformat");
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        "blake3",
        "clone must init from discovery blake3 kind"
    );
    let log = run_libra_command(&["log", "--oneline", "-1"], &dest);
    assert_cli_success(&log, "blake3 clone log");
}

#[test]
#[serial(cwd)]
fn clone_sha256_libra_local_remote_succeeds() {
    use crate::command::{
        assert_cli_success, create_committed_repo_with_format, run_libra_command,
    };

    let source = create_committed_repo_with_format("sha256");
    let dest_root = tempdir().expect("sha256 clone dest root");
    let dest = dest_root.path().join("sha256-clone");
    let out = run_libra_command(
        &[
            "clone",
            source.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_cli_success(&out, "sha256 Libra local clone positive control");
    let format = run_libra_command(&["config", "--get", "core.objectformat"], &dest);
    assert_cli_success(&format, "read sha256 objectformat");
    assert_eq!(String::from_utf8_lossy(&format.stdout).trim(), "sha256");
}

/// B3-12: rewrite a local Git repo's config to advertise `objectformat`.
fn force_git_source_objectformat(repo: &std::path::Path, value: &str) {
    let git_config = repo.join(".git").join("config");
    let mut text = fs::read_to_string(&git_config).expect("read git config");
    if text.contains("repositoryformatversion = 0") {
        text = text.replace("repositoryformatversion = 0", "repositoryformatversion = 1");
    } else if !text.contains("repositoryformatversion = 1") {
        text.push_str("\n[core]\n\trepositoryformatversion = 1\n");
    }
    text.push_str(&format!("\n[extensions]\n\tobjectformat = {value}\n"));
    fs::write(&git_config, text).expect("write git config");
}

#[test]
#[serial(cwd)]
fn clone_rejects_sha256_git_source() {
    use crate::command::{
        assert_cli_success, create_linear_git_repo, parse_cli_error_stderr, run_libra_command,
    };

    // Positive control: unmodified SHA-1 Git source still clones (AC10).
    let (sha1_src, _) = create_linear_git_repo(1);
    let sha1_dest_root = tempdir().expect("sha1 clone dest root");
    let sha1_dest = sha1_dest_root.path().join("sha1-ok");
    let ok = run_libra_command(
        &[
            "clone",
            sha1_src.path().to_str().unwrap(),
            sha1_dest.to_str().unwrap(),
        ],
        sha1_dest_root.path(),
    );
    assert_cli_success(&ok, "SHA-1 local Git clone positive control");
    assert!(
        sha1_dest.join(".libra").exists(),
        "sha1 clone writes .libra"
    );

    let (git_src, _) = create_linear_git_repo(1);
    force_git_source_objectformat(git_src.path(), "sha256");

    let dest_root = tempdir().expect("sha256 reject dest root");
    let dest = dest_root.path().join("sha256-reject");
    let out = run_libra_command(
        &[
            "--json",
            "clone",
            git_src.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        dest_root.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(129),
        "exit: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let (_human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002", "{report:?}");
    assert!(
        report.hints.iter().any(|h| h.contains("fresh")
            || h.contains("libra init --object-format")
            || h.contains("SHA-256/BLAKE3")),
        "hint must point at fresh init: {:?}",
        report.hints
    );
    assert!(!dest.join(".libra").exists(), "zero-write: no .libra");
    assert!(!dest.exists() || fs::read_dir(&dest).map(|d| d.count()).unwrap_or(0) == 0);
}

#[test]
#[serial(cwd)]
fn clone_rejects_unknown_git_source_format() {
    use crate::command::{create_linear_git_repo, parse_cli_error_stderr, run_libra_command};

    // Unknown objectformat → LBR-REPO-002 / 128, zero write.
    {
        let (git_src, _) = create_linear_git_repo(1);
        force_git_source_objectformat(git_src.path(), "not-a-hash");
        let dest_root = tempdir().expect("unknown format dest root");
        let dest = dest_root.path().join("unknown-reject");
        let out = run_libra_command(
            &[
                "--json",
                "clone",
                git_src.path().to_str().unwrap(),
                dest.to_str().unwrap(),
            ],
            dest_root.path(),
        );
        assert_eq!(out.status.code(), Some(128), "{:?}", out);
        let (_human, report) = parse_cli_error_stderr(&out.stderr);
        assert_eq!(report.error_code, "LBR-REPO-002", "{report:?}");
        assert!(!dest.join(".libra").exists(), "zero-write: no .libra");
    }

    // Unreadable config → LBR-IO-001 / 128, zero write.
    {
        let (git_src, _) = create_linear_git_repo(1);
        let git_config = git_src.path().join(".git").join("config");
        fs::remove_file(&git_config).expect("remove config");
        fs::create_dir(&git_config).expect("directory-as-config");
        let dest_root = tempdir().expect("unreadable config dest root");
        let dest = dest_root.path().join("unreadable-reject");
        let out = run_libra_command(
            &[
                "--json",
                "clone",
                git_src.path().to_str().unwrap(),
                dest.to_str().unwrap(),
            ],
            dest_root.path(),
        );
        assert_eq!(out.status.code(), Some(128), "{:?}", out);
        let (_human, report) = parse_cli_error_stderr(&out.stderr);
        assert_eq!(report.error_code, "LBR-IO-001", "{report:?}");
        assert!(!dest.join(".libra").exists(), "zero-write: no .libra");
    }
}

/// Build a Git source with a single commit containing `file_count` distinct
/// root blobs. Reaching and encoding all of them yields far more entries than
/// the sum of the two bounded channel capacities (1_000 + 1_000 = 2_000), which
/// is exactly the regime in which the pre-fix in-house encoder in
/// `local_client.rs::encode_pack_bytes` deadlocked the local-path clone at
/// "Fetching objects" (0% CPU). This is much faster than a long linear history
/// while still exercising the ADR-CLH-01 fix end-to-end.
fn create_wide_git_repo(file_count: usize) -> tempfile::TempDir {
    assert!(file_count > 2_000, "must exceed both channel capacities");
    let dir = tempdir().expect("wide git tempdir");
    let repo = dir.path();
    super::git_success(repo, &["init", "-b", "main"]);
    super::git_success(repo, &["config", "user.name", "Wide Tester"]);
    super::git_success(repo, &["config", "user.email", "wide@test"]);
    super::git_success(repo, &["config", "commit.gpgsign", "false"]);
    for i in 0..file_count {
        fs::write(repo.join(format!("f{i:05}.txt")), format!("content {i}\n"))
            .expect("write wide source file");
    }
    super::git_success(repo, &["add", "-A"]);
    super::git_success(repo, &["commit", "-m", "wide single commit"]);
    dir
}

/// Build a DEEP local source: `commit_count` linear commits written through
/// `fast-import`, i.e. two objects per commit (blob + commit). 1 200 commits is
/// 2 400 objects, which already crosses the > ~2 000 channel-capacity sum that
/// deadlocked the pre-fix encoder (ADR-CLH-01), while staying fast enough for a
/// normal integration run.
fn create_deep_git_repo(commit_count: usize) -> tempfile::TempDir {
    assert!(commit_count > 1_000, "must exceed both channel capacities");
    let dir = tempdir().expect("deep git tempdir");
    let repo = dir.path();
    // The reported reproduction is a LIBRA source (`libra init` + `libra
    // fast-import`), so the clone exercises the Libra local transport where the
    // encoder deadlock lived.
    super::assert_cli_success(
        &super::run_libra_command(&["init", "--quiet", "."], repo),
        "init the deep source repository",
    );

    let mut stream = String::new();
    for i in 1..=commit_count {
        let timestamp = 1_700_000_000 + (i as i64) * 60;
        stream.push_str(&format!(
            "blob\nmark :{blob}\ndata {len}\n{i}\n\n\
             commit refs/heads/main\nmark :{commit}\n\
             author T <t@example.com> {timestamp} +0000\n\
             committer T <t@example.com> {timestamp} +0000\n\
             data {msg_len}\nc{i}\n",
            blob = i * 2 - 1,
            commit = i * 2,
            len = i.to_string().len() + 1,
            msg_len = format!("c{i}").len() + 1,
        ));
        if i > 1 {
            stream.push_str(&format!("from :{}\n", (i - 1) * 2));
        }
        stream.push_str(&format!("M 100644 :{} f.txt\n\n", i * 2 - 1));
    }
    let stream_path = dir.path().join("stream.fi");
    fs::write(&stream_path, stream).expect("write fast-import stream");

    super::assert_cli_success(
        &super::run_libra_command(
            &[
                "fast-import",
                "--quiet",
                "--input",
                stream_path.to_str().expect("stream path"),
            ],
            repo,
        ),
        "fast-import the deep history",
    );
    dir
}

/// Issue #496 regression guard (deep-history shape): the reported reproduction
/// is a LOCAL clone of a 3 200-commit linear history, which hung at "Fetching
/// objects" with 0% CPU. The wide fixture above covers the object-count
/// trigger; this one covers the reported history shape end to end, including
/// the post-clone rev walk and working-tree checkout.
#[test]
fn test_clone_local_deep_history_completes_in_budget() {
    let commit_count = 1_200;
    let deep = create_deep_git_repo(commit_count);
    let source = deep.path().to_str().expect("source path").to_string();
    let dest_root = tempdir().expect("dest root");
    let dest = dest_root.path().join("deep-clone");
    let dest_str = dest.to_str().expect("dest path").to_string();

    let (out, timed_out) = run_libra_command_with_timeout(
        &["clone", &source, &dest_str],
        dest_root.path(),
        Duration::from_secs(60),
    );
    assert!(
        !timed_out,
        "local clone of a {commit_count}-commit history hung at Fetching objects: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    super::assert_cli_success(&out, "deep local clone should complete");

    let count = super::run_libra_command(&["rev-list", "--count", "HEAD"], &dest);
    super::assert_cli_success(&count, "rev-list --count on the clone");
    assert_eq!(
        String::from_utf8_lossy(&count.stdout).trim(),
        commit_count.to_string(),
        "every commit was transferred"
    );
    let status = super::run_libra_command(&["status", "--porcelain"], &dest);
    super::assert_cli_success(&status, "status on the clone");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "the checked-out working tree must be clean: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

/// Mega2 code-view clone planning excludes traces refs. Ordinary Git mirrors
/// keep the full advertised namespace. A traces locator never reaches discovery.
#[test]
fn mega2_code_clone_excludes_traces_matrix() {
    use libra::internal::protocol::mega2_traces::{
        code_view_keeps_ref, is_mega2_code_view_url, is_traces_locator_url, mirror_fetch_refspec,
    };

    use super::run_libra_command;

    let https = "https://mega.example/project/foo.git";
    let http = "http://127.0.0.1:8080/project/foo.git";
    let ssh = "ssh://git@mega.example/project/foo.git";
    let traces = "https://mega.example/.traces/by-scope/11111111-1111-4111-8111-111111111111.git";
    let github = "ssh://git@github.com/org/repo.git";
    let local = "/tmp/project/repo.git";

    for url in [https, http, ssh] {
        assert!(is_mega2_code_view_url(url), "{url}");
        assert_eq!(mirror_fetch_refspec(url), "+refs/heads/*:refs/heads/*");
    }
    assert!(is_traces_locator_url(traces));
    assert!(!is_mega2_code_view_url(traces));
    assert!(!is_mega2_code_view_url(github));
    assert!(!is_mega2_code_view_url(local));
    assert_eq!(mirror_fetch_refspec(github), "+refs/*:refs/*");
    assert_eq!(mirror_fetch_refspec(local), "+refs/*:refs/*");
    assert_eq!(
        mirror_fetch_refspec("git@github.com:org/repo.git"),
        "+refs/*:refs/*"
    );

    assert!(code_view_keeps_ref("refs/heads/main"));
    assert!(code_view_keeps_ref("refs/tags/v1"));
    assert!(code_view_keeps_ref("HEAD"));
    assert!(!code_view_keeps_ref("refs/libra/traces"));
    assert!(!code_view_keeps_ref("refs/libra/traces/extra"));

    let root = tempdir().expect("clone root");
    let destination = root.path().join("dest");
    let output = run_libra_command(
        &[
            "clone",
            traces,
            destination.to_str().expect("utf8 destination"),
        ],
        root.path(),
    );
    assert!(
        !output.status.success(),
        "a traces locator must be refused before discovery"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("traces locator"),
        "refusal should name the traces locator: {stderr}"
    );
    assert!(
        !destination.exists(),
        "refused clone must not create {destination:?}"
    );

    let source = tempdir().expect("ordinary source");
    assert!(run_libra_command(&["init"], source.path()).status.success());
    assert!(
        run_libra_command(&["config", "set", "user.name", "Clone"], source.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(
            &["config", "set", "user.email", "clone@example.com"],
            source.path()
        )
        .status
        .success()
    );
    fs::write(source.path().join("code.txt"), "code\n").expect("code file");
    assert!(
        run_libra_command(&["add", "code.txt"], source.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "code", "--no-verify"], source.path())
            .status
            .success()
    );
    let mirror = root.path().join("ordinary-mirror");
    let mirrored = run_libra_command(
        &[
            "clone",
            "--mirror",
            source.path().to_str().expect("source path"),
            mirror.to_str().expect("mirror path"),
        ],
        root.path(),
    );
    assert!(
        mirrored.status.success(),
        "ordinary mirror failed: {}",
        String::from_utf8_lossy(&mirrored.stderr)
    );
    let config = run_libra_command(&["config", "list"], &mirror);
    let config_text = String::from_utf8_lossy(&config.stdout);
    assert!(
        config_text.contains("+refs/*:refs/*"),
        "a non-Mega2 mirror keeps every advertised ref: {config_text}"
    );
}

/// Run the Libra binary via `base_libra_command`'s isolated HOME, but poll it
/// with a hard deadline: return `(output, timed_out)`. If the clone deadlocks
/// (the ADR-CLH-01 regression), the child never exits, `timed_out` becomes
/// `true` and the test fails instead of hanging the whole suite.
fn run_libra_command_with_timeout(
    args: &[&str],
    cwd: &Path,
    budget: Duration,
) -> (std::process::Output, bool) {
    use std::process::Stdio;

    let mut child = super::base_libra_command(args, cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn libra binary");
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child.wait_with_output().expect("failed to collect output");
                return (output, false);
            }
            Ok(None) => {
                if start.elapsed() > budget {
                    let _ = child.kill();
                    let output = child.wait_with_output().expect("failed to collect output");
                    return (output, true);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("failed to poll libra child: {error}"),
        }
    }
}

/// ADR-CLH-01 regression guard: a local-path clone of a source with more than
/// the channel-capacity object count must complete inside a generous time
/// budget rather than hang at "Fetching objects" (0% CPU).
#[test]
fn test_clone_local_git_large_object_set_completes_in_budget() {
    use super::run_libra_command;

    let file_count = 2_500;
    let wide = create_wide_git_repo(file_count);
    let source = wide.path().to_str().unwrap().to_string();
    let dest_root = tempdir().expect("dest root");
    let dest = dest_root.path().join("wide-clone");
    let dest_str = dest.to_str().unwrap().to_string();

    let (out, timed_out) = run_libra_command_with_timeout(
        &["clone", &source, &dest_str],
        dest_root.path(),
        Duration::from_secs(60),
    );
    assert!(
        !timed_out,
        "local clone of {file_count}-blob source hung at Fetching objects: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    super::assert_cli_success(&out, "large local clone should complete");
    assert!(dest.join(".libra").exists(), "clone created a repo");

    // Integrity: all objects were encoded and the working tree was materialized.
    let log = run_libra_command(&["log", "--oneline"], &dest);
    super::assert_cli_success(&log, "log on cloned repo");
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).lines().count(),
        1,
        "single-commit history cloned exactly once"
    );
    let checked_out = fs::read_dir(&dest)
        .expect("read cloned working tree")
        .count();
    assert!(
        checked_out >= file_count,
        "expected all {file_count} wide files checked out, found {checked_out}"
    );
    let status = run_libra_command(&["status", "--short", "--branch"], &dest);
    super::assert_cli_success(&status, "status on cloned repo");
}

fn storage_config_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
#[serial(cwd, env)]
fn storage_config_failure_hides_values_clone() {
    let source = super::create_committed_repo_via_cli();
    let parent = tempdir().expect("temp");
    let dest = parent.path().join("dest");
    let canary = "canary-secret-not-a-url";
    let output = super::run_libra_command_with_env(
        &[
            "--json",
            "clone",
            source.path().to_str().expect("utf8"),
            dest.to_str().expect("utf8"),
        ],
        parent.path(),
        &[
            ("LIBRA_STORAGE_TYPE", "s3"),
            ("LIBRA_STORAGE_BUCKET", "bucket"),
            ("LIBRA_STORAGE_ENDPOINT", canary),
        ],
    );
    let text = storage_config_text(&output);
    assert!(!text.contains(canary), "{text}");
    assert!(!output.status.success(), "{text}");
    let (_, report) = super::parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-IO-001", "{report:?}");
    if dest.join(".libra").exists() {
        let pack = dest.join(".libra").join("objects").join("pack");
        if pack.exists() {
            let found = fs::read_dir(&pack)
                .expect("pack")
                .flatten()
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"));
            assert!(!found, "clone must not install a pack");
        }
    }
}

#[test]
#[serial(cwd, env)]
fn storage_config_unresolved_fetch_stays_local_clone() {
    let source = super::create_committed_repo_via_cli();
    let parent = tempdir().expect("temp");
    let dest = parent.path().join("dest");
    let output = super::run_libra_command_with_env(
        &[
            "clone",
            source.path().to_str().expect("utf8"),
            dest.to_str().expect("utf8"),
        ],
        parent.path(),
        &[(
            "LIBRA_CONFIG_GLOBAL_DB",
            parent
                .path()
                .join("missing-global.db")
                .to_str()
                .expect("utf8"),
        )],
    );
    let text = storage_config_text(&output);
    assert!(
        !text.contains("failed to read object storage configuration"),
        "{text}"
    );
    assert!(output.status.success(), "{text}");
}

#[test]
fn clone_reference_registers_the_object_alternate() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let reference = root.path().join("R");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                src.path().to_str().expect("src"),
                reference.to_str().expect("reference"),
            ],
            root.path(),
        ),
        "clone the reference repository",
    );

    // AC1: `--reference` records the reference's object store.
    let d = root.path().join("d");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--reference",
                reference.to_str().expect("reference"),
                &format!("file://{}", src.path().display()),
                d.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "clone --reference",
    );
    let alternates = std::fs::read_to_string(d.join(".libra/objects/info/alternates"))
        .expect("the reference alternate is recorded");
    let recorded = alternates.lines().next().expect("one alternate").trim();
    assert_eq!(
        std::fs::canonicalize(recorded).expect("canonical alternate"),
        std::fs::canonicalize(reference.join(".libra/objects"))
            .expect("canonical reference objects"),
        "AC1: the alternate points at the reference object store"
    );

    // AC5: `--reference-if-able` with a missing path is a silent no-op.
    let d2 = root.path().join("d2");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--reference-if-able",
                root.path().join("missing").to_str().expect("missing"),
                &format!("file://{}", src.path().display()),
                d2.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "clone --reference-if-able with a missing path",
    );
    assert!(
        !d2.join(".libra/objects/info/alternates").exists(),
        "AC5: no alternate is registered for a missing reference"
    );
}

#[test]
fn clone_reference_dissociate_is_self_contained() {
    use crate::command::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let src = create_committed_repo_via_cli();
    let root = tempdir().unwrap();
    let reference = root.path().join("R");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                src.path().to_str().expect("src"),
                reference.to_str().expect("reference"),
            ],
            root.path(),
        ),
        "clone the reference repository",
    );

    // AC1/AC2: `--dissociate` copies instead of borrowing.
    let d = root.path().join("d");
    let out = run_libra_command(
        &[
            "clone",
            "--json",
            "--reference",
            reference.to_str().expect("reference"),
            "--dissociate",
            &format!("file://{}", src.path().display()),
            d.to_str().expect("dest"),
        ],
        root.path(),
    );
    assert_cli_success(&out, "clone --reference --dissociate");
    assert!(
        !d.join(".libra/objects/info/alternates").exists(),
        "AC1: --dissociate must not register an alternate"
    );
    let payload = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(payload.trim()).expect("clone json");
    assert!(
        value["data"]["objects_fetched"].as_u64().unwrap_or(0) > 0,
        "AC2: --dissociate transfers the objects: {payload}"
    );

    // AC3: the clone survives deleting the reference.
    std::fs::remove_dir_all(&reference).expect("remove the reference");
    assert_cli_success(
        &run_libra_command(&["fsck"], &d),
        "AC3: fsck after deleting the reference",
    );

    // AC4/AC5: `--shared --dissociate` is self-contained too.
    let src2 = create_committed_repo_via_cli();
    let d2 = root.path().join("d2");
    assert_cli_success(
        &run_libra_command(
            &[
                "clone",
                "--shared",
                "--dissociate",
                src2.path().to_str().expect("src"),
                d2.to_str().expect("dest"),
            ],
            root.path(),
        ),
        "clone --shared --dissociate",
    );
    assert!(
        !d2.join(".libra/objects/info/alternates").exists(),
        "AC4: --shared --dissociate must not register an alternate"
    );
    drop(src2);
    assert_cli_success(
        &run_libra_command(&["fsck"], &d2),
        "AC5: fsck after deleting the source",
    );
}
